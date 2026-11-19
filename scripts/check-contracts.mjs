#!/usr/bin/env node
/**
 * 契约检查（C1–C4）。零依赖，只在仓库内只读扫描，不改任何文件。
 * 覆盖 docs/纪律清单.md 里能被机械验证的条目。
 *
 * 用法：npm run contract
 */
import { readFileSync, readdirSync, statSync } from 'node:fs';
import { join, relative, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = fileURLToPath(new URL('..', import.meta.url));
const rel = (p) => relative(root, p).split(sep).join('/');

const failures = [];
const passes = [];
function check(id, title, run) {
  try {
    const notes = run();
    passes.push(id + ' ' + title + (notes && notes.length ? '（' + notes.join('，') + '）' : ''));
  } catch (err) {
    failures.push(id + ' ' + title + ' :: ' + err.message);
  }
}
function fail(msg) {
  throw new Error(msg);
}

function walk(dir, exts, out) {
  let entries;
  try {
    entries = readdirSync(dir, { withFileTypes: true });
  } catch {
    return out;
  }
  for (const entry of entries) {
    const full = join(dir, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'node_modules' || entry.name === 'dist' || entry.name === 'target') continue;
      walk(full, exts, out);
    } else if (exts.some((e) => entry.name.endsWith(e))) {
      out.push(full);
    }
  }
  return out;
}

// C1 单一 IPC 出口：只有 src/lib/ipc.ts 能碰 invoke。
check('C1', '渲染进程只有一个后端出口', () => {
  const files = walk(join(root, 'src'), ['.ts', '.tsx'], []);
  const offenders = [];
  for (const file of files) {
    const r = rel(file);
    if (r === 'src/lib/ipc.ts') continue;
    const text = readFileSync(file, 'utf8');
    if (/\binvoke\s*\(/.test(text) || text.includes('@tauri-apps/api/core')) offenders.push(r);
  }
  if (offenders.length) fail('绕过 api 直接调用 invoke：' + offenders.join('、'));
  return ['扫描 ' + files.length + ' 个前端文件'];
});

// C2 命令必须注册：commands 里每个 #[tauri::command] 都要进 lib.rs 的 generate_handler!。
check('C2', '后端命令全部注册', () => {
  const files = walk(join(root, 'src-tauri', 'src', 'commands'), ['.rs'], []);
  const declared = new Map();
  for (const file of files) {
    const text = readFileSync(file, 'utf8');
    // 两种写法都合法：#[tauri::command] 全路径，或 use tauri::command; 之后的 #[command]。
    const re = /#\[(?:tauri::)?command(?:\s*\([^)]*\))?\][\s\S]{0,400}?pub\s+(?:async\s+)?fn\s+(\w+)/g;
    let m;
    while ((m = re.exec(text))) declared.set(m[1], rel(file));
  }
  const lib = readFileSync(join(root, 'src-tauri', 'src', 'lib.rs'), 'utf8');
  const block = lib.match(/generate_handler!\[([\s\S]*?)\]/);
  if (!block) fail('lib.rs 里找不到 generate_handler! 列表');
  const registered = new Set(block[1].split(',').map((s) => s.trim().split('::').pop().trim()).filter(Boolean));
  const missing = [];
  for (const [name, where] of declared) if (!registered.has(name)) missing.push(name + '@' + where);
  if (missing.length) fail('声明了但没注册：' + missing.join('、'));
  const orphans = [...registered].filter((n) => !declared.has(n));
  if (orphans.length) fail('注册了但找不到声明：' + orphans.join('、'));
  return ['命令 ' + declared.size + ' 个'];
});

// C3 版本号三处一致。
check('C3', '版本号三处一致', () => {
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8')).version;
  const conf = JSON.parse(readFileSync(join(root, 'src-tauri', 'tauri.conf.json'), 'utf8')).version;
  const cargo = readFileSync(join(root, 'src-tauri', 'Cargo.toml'), 'utf8');
  const cm = cargo.match(/^version\s*=\s*"([^"]+)"/m);
  const cargoV = cm ? cm[1] : null;
  if (!(pkg === conf && conf === cargoV)) {
    fail('package.json=' + pkg + ' tauri.conf.json=' + conf + ' Cargo.toml=' + cargoV);
  }
  return ['v' + pkg];
});

// C4 规格镜像：文档里的句式表必须与 pattern_defs() 一一对应（顺序、数量、命名）。
check('C4', '文风判据表与 pattern_defs() 一致', () => {
  const doc = readFileSync(join(root, 'docs', '文风统计.md'), 'utf8');
  const seg = doc.split('<!-- patterns:begin -->')[1];
  if (!seg) fail('文档里缺少 patterns:begin/end 标记段');
  const body = seg.split('<!-- patterns:end -->')[0];
  const docNames = body
    .split('\n')
    .filter((l) => l.startsWith('| '))
    .map((l) => l.split('|')[1].trim())
    .filter((cell) => cell && cell !== '句式名（代码里逐字一致）' && !cell.startsWith('---'));
  const code = readFileSync(join(root, 'src-tauri', 'src', 'services', 'stylestat.rs'), 'utf8');
  const defsBlock = code.match(/let raw: \[\(&str, &str\); (\d+)\] = \[([\s\S]*?)\n\s*\];/);
  if (!defsBlock) fail('stylestat.rs 里找不到 pattern_defs() 的 raw 数组');
  // 数组里既有单行 entry，也有跨行 entry：取每个元素开头的第一个字符串字面量。
  const codeNames = [...defsBlock[2].matchAll(/\(\s*"([^"]+)"/g)].map((m) => m[1]);
  if (docNames.length !== codeNames.length) {
    fail('条数不一致：文档 ' + docNames.length + ' 条，代码 ' + codeNames.length + ' 条');
  }
  const diff = [];
  docNames.forEach((name, i) => {
    if (name !== codeNames[i]) diff.push('第' + (i + 1) + '条 文档「' + name + '」≠ 代码「' + codeNames[i] + '」');
  });
  if (diff.length) fail(diff.join('；'));
  return ['8 条判据逐字一致'.replace('8', String(codeNames.length))];
});

for (const line of passes) console.log('  PASS  ' + line);
for (const line of failures) console.log('  FAIL  ' + line);
if (failures.length) {
  console.log('\n契约检查未通过：' + failures.length + ' 项（见 docs/纪律清单.md）');
  process.exit(1);
}
console.log('\n契约检查通过：' + passes.length + ' 项');
