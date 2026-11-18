import type { TreeNode } from "@/types";

/** 上下文清单上限（与 Rust `engine::CONTEXT_LIMIT` 保持一致）。 */
export const CONTEXT_LIMIT = 40;

function findDir(node: TreeNode, dir: string): TreeNode | null {
  if (node.kind === "dir" && (node.name === dir || node.rel === dir)) return node;
  for (const child of node.children) {
    const hit = findDir(child, dir);
    if (hit) return hit;
  }
  return null;
}

function walk(node: TreeNode, out: string[], limit: number) {
  for (const child of node.children) {
    if (out.length >= limit) return;
    if (child.kind === "dir") {
      walk(child, out, limit);
    } else if (child.rel.toLowerCase().endsWith(".md")) {
      out.push(child.rel);
    }
  }
}

/**
 * 从文件树里抓「聚焦目录」下的 Markdown 文件（相对书根的路径）。
 *
 * 只取文件名清单，不读内容：正文可能有几十万字，塞进 prompt 会直接把上下文打满；
 * 智能体会自己按清单去读需要的文件（这是 oh-story 的既有工作方式）。
 */
export function collectContextFiles(
  tree: TreeNode | null,
  dir: string,
  limit = CONTEXT_LIMIT,
): string[] {
  if (!tree) return [];
  const node = findDir(tree, dir);
  if (!node) return [];
  const out: string[] = [];
  walk(node, out, limit);
  return out;
}
