/**
 * 字数统计：中文字符按「字」计，西文按「词」计。
 *
 * 与 `storyctl.py wordcount` 的口径一致：它统计的是正文里的汉字数，
 * 这里额外把英文单词/数字串也算进来，给作者一个总览（状态栏会分开显示）。
 */

export interface WordCount {
  /** 汉字/中文标点之外的汉字字符数（逐字计数） */
  cjk: number;
  /** 西文单词与数字串个数 */
  words: number;
  /** 字符总数（含标点、空白，按 Unicode 码点计） */
  chars: number;
  /** cjk + words */
  total: number;
}

/** CJK 统一表意文字（含扩展 A、兼容区）与中日韩标点里的汉字主体。 */
function isCjk(code: number): boolean {
  return (
    (code >= 0x3400 && code <= 0x4dbf) || // 扩展 A
    (code >= 0x4e00 && code <= 0x9fff) || // 基本区
    (code >= 0xf900 && code <= 0xfaff) || // 兼容表意文字
    (code >= 0x3040 && code <= 0x30ff) // 日文假名（中文小说里偶尔出现）
  );
}

/** 西文单词字符：字母、数字、下划线。 */
function isWordChar(code: number): boolean {
  return (
    (code >= 0x30 && code <= 0x39) || // 0-9
    (code >= 0x41 && code <= 0x5a) || // A-Z
    (code >= 0x61 && code <= 0x7a) || // a-z
    code === 0x5f || // _
    (code >= 0xc0 && code <= 0x24f) // 拉丁扩展（é、ü 等）
  );
}

export function countWords(text: string): WordCount {
  let cjk = 0;
  let words = 0;
  let chars = 0;
  let inWord = false;

  for (const ch of text) {
    chars += 1;
    const code = ch.codePointAt(0) ?? 0;

    if (isCjk(code)) {
      cjk += 1;
      inWord = false;
      continue;
    }

    if (isWordChar(code)) {
      if (!inWord) {
        words += 1;
        inWord = true;
      }
      continue;
    }

    inWord = false;
  }

  return { cjk, words, chars, total: cjk + words };
}

/** 状态栏展示用：`1,234 字`。 */
export function formatCount(n: number): string {
  return n.toLocaleString("zh-CN");
}
