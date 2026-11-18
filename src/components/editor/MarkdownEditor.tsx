import { useEffect, useRef } from "react";

import { defaultKeymap, history, historyKeymap, indentWithTab } from "@codemirror/commands";
import { markdown, markdownLanguage } from "@codemirror/lang-markdown";
import { HighlightStyle, syntaxHighlighting } from "@codemirror/language";
import { highlightSelectionMatches, searchKeymap } from "@codemirror/search";
import { EditorState, type Extension } from "@codemirror/state";
import {
  EditorView,
  drawSelection,
  highlightActiveLine,
  keymap,
  placeholder,
} from "@codemirror/view";
import { tags } from "@lezer/highlight";

import { useEditorStore } from "@/store/editorStore";

/** 写作界面主题：衬线正文、行高 1.9、居中的 780px 文本栏。 */
const writingTheme = EditorView.theme({
  "&": {
    height: "100%",
    fontSize: "15px",
    backgroundColor: "#ffffff",
    color: "#1f2430",
  },
  "&.cm-focused": { outline: "none" },
  ".cm-scroller": {
    fontFamily:
      '"LXGW WenKai", "Noto Serif SC", "Source Han Serif SC", "Songti SC", Georgia, serif',
    lineHeight: "1.9",
    overflowY: "auto",
  },
  ".cm-content": {
    maxWidth: "820px",
    margin: "0 auto",
    padding: "26px 40px 140px 40px",
    caretColor: "#2f6df6",
  },
  ".cm-line": { padding: "0" },
  ".cm-activeLine": { backgroundColor: "rgba(47,109,246,0.04)" },
  ".cm-selectionBackground, ::selection": { backgroundColor: "rgba(47,109,246,0.18) !important" },
  ".cm-cursor": { borderLeftColor: "#2f6df6" },
  ".cm-placeholder": { color: "#b6bdc9", fontStyle: "italic" },
});

const markdownHighlight = HighlightStyle.define([
  { tag: tags.heading1, fontSize: "1.45em", fontWeight: "700", color: "#101420" },
  { tag: tags.heading2, fontSize: "1.26em", fontWeight: "700", color: "#161c28" },
  { tag: tags.heading3, fontSize: "1.12em", fontWeight: "600", color: "#1c2331" },
  { tag: tags.heading4, fontWeight: "600" },
  { tag: tags.strong, fontWeight: "700" },
  { tag: tags.emphasis, fontStyle: "italic" },
  { tag: tags.strikethrough, textDecoration: "line-through", color: "#98a1b0" },
  { tag: tags.link, color: "#2f6df6", textDecoration: "underline" },
  { tag: tags.url, color: "#2f6df6" },
  { tag: tags.monospace, fontFamily: "ui-monospace, SFMono-Regular, Consolas, monospace", backgroundColor: "#f4f5f8" },
  { tag: tags.quote, color: "#5b6573", fontStyle: "italic" },
  { tag: tags.list, color: "#33404f" },
  { tag: [tags.meta, tags.processingInstruction], color: "#a3aab7" },
  { tag: tags.comment, color: "#8c94a3" },
]);

/**
 * 编辑器扩展集合：首次创建与「外部灌入后重建 state」共用同一份，避免两处配置漂移。
 */
function buildExtensions(readOnly: boolean, applying: { current: boolean }): Extension[] {
  return [
    history(),
    drawSelection(),
    highlightActiveLine(),
    highlightSelectionMatches(),
    EditorView.lineWrapping,
    placeholder("在这里写正文…… 停手 1 秒自动保存，Ctrl/Cmd + S 立刻保存"),
    markdown({ base: markdownLanguage, addKeymap: true }),
    syntaxHighlighting(markdownHighlight),
    writingTheme,
    keymap.of([
      {
        key: "Mod-s",
        preventDefault: true,
        run: () => {
          void useEditorStore.getState().saveNow();
          return true;
        },
      },
      indentWithTab,
      ...historyKeymap,
      ...searchKeymap,
      ...defaultKeymap,
    ]),
    EditorState.readOnly.of(readOnly),
    EditorView.editable.of(!readOnly),
    EditorView.updateListener.of((update) => {
      // 外部灌入不算用户输入：那条路径会整份重建 state，撤销历史随之清空
      if (!update.docChanged || applying.current) return;
      useEditorStore.getState().setContent(update.state.doc.toString());
    }),
  ];
}

/**
 * Markdown 编辑器（CodeMirror 6）。
 *
 * 职责边界：
 * - 只把「内容变化」写进 store，**不负责保存**——保存时机由 store 的防抖决定；
 * - store 里的内容被外部改掉（切文件、冲突后重新加载）时，单向同步进编辑器，
 *   用 `applying` 标志避免把这次同步再当成用户输入；
 * - 外部灌入**整份重建 EditorState**（见下方第二个 effect），不追加历史。
 */
export function MarkdownEditor({ readOnly = false }: { readOnly?: boolean }) {
  const host = useRef<HTMLDivElement | null>(null);
  const view = useRef<EditorView | null>(null);
  const applying = useRef(false);

  const content = useEditorStore((s) => s.content);

  useEffect(() => {
    const parent = host.current;
    if (!parent) return;

    const state = EditorState.create({
      doc: useEditorStore.getState().content,
      extensions: buildExtensions(readOnly, applying),
    });

    const instance = new EditorView({ state, parent });
    view.current = instance;

    return () => {
      instance.destroy();
      view.current = null;
    };
    // readOnly 变化时重建编辑器：写作区与只读区切换属于低频操作
  }, [readOnly]);

  // 外部内容变化（切文件 / 冲突重新加载 / 恢复快照）单向同步进编辑器。
  //
  // 这里必须整份重建 EditorState，不能 dispatch 全量替换：history() 的撤销栈挂在
  // EditorState 上，dispatch 只会把这次替换当成「又一步编辑」追加进去，旧文件的历史
  // 依然可撤销——切到 B 后按一次 Ctrl+Z 就退回 A 的正文，再被 1 秒防抖自动保存写进
  // B（跨文件数据损坏）。setState 换掉整个 state（含撤销栈），外部内容直接成为新基线；
  // 用户此后自己的输入照常可撤销。
  useEffect(() => {
    const instance = view.current;
    if (!instance) return;
    if (instance.state.doc.toString() === content) return;

    applying.current = true;
    try {
      instance.setState(
        EditorState.create({
          doc: content,
          extensions: buildExtensions(readOnly, applying),
        }),
      );
    } finally {
      applying.current = false;
    }
  }, [content, readOnly]);

  return <div ref={host} className="h-full min-h-0 w-full overflow-hidden bg-white" />;
}
