//! 乱码识别：认出「UTF-8 字节被按 GBK/cp936 解码后又写回」的文本。
//!
//! 事故背景（第 037 章）：写手用 Windows PowerShell 5.1 改稿，`Get-Content -Raw` 对
//! **无 BOM 的 UTF-8** 文件按系统 ANSI（简体中文机器上是 cp936）解码，中文全部变成
//! `绗?37绔?琛ヤ綅浜` 这种乱码；`Set-Content -Encoding UTF8` 再把乱码写回去并加上 BOM。
//! 结果是文件本身是**合法 UTF-8**（所以 [`super::atomic_write::read_text`] 照读不误），
//! 内容却已经不可逆损坏——GBK 解码时丢掉的字节变成 `?`，反解不回来。
//!
//! 因此这一层的职责只有一件事：**不让它静默通过**。检测到就报出来，让作者知道该重写这一章，
//! 而不是在界面上看到一片「绗?37绔?」还以为是字体问题。
//!
//! 判定不看 BOM（app 自己写文件从不带 BOM，但别人的编辑器会带），只看字符分布：
//! 这些标记字（`绗 锛 銆 鈥 鏄 鐨 浜 …`）是 cp936 解 UTF-8 的典型残留，正常中文里几乎不出现。

/// cp936 误解码 UTF-8 的高频残留字。正常中文正文里它们几乎不会成片出现。
const MOJIBAKE_MARKERS: &str =
    "绗锛銆鈥鏄鐨浜涓浠鍦鍚鐪璇鏃鐢鍙鎴寰鎶鍜鐩澶鏋琛绔鑹鍐閭閿鍑鏈缁勫墽鍏虫溇閫夋嫨";

/// 这段文本像不像「UTF-8 被按 GBK 解码」的乱码。
///
/// - 含私有区字符（U+E000–U+F8FF）：cp936 的用户自定义区会造出这种字，正常中文不会有 → 判乱码。
/// - 标记字占比 ≥ 20%：正常中文里这些字占比远低于 1%。
/// - 太短的文本不下结论（比如只有一句台词），避免误伤。
pub fn looks_like_mojibake(text: &str) -> bool {
    // 私有区字符是硬证据：cp936 的用户自定义区会造出这种字，正常中文一个都不会有。
    if text.chars().any(|c| ('\u{e000}'..='\u{f8ff}').contains(&c)) {
        return true;
    }
    let total = text.chars().filter(|c| !c.is_whitespace()).count();
    if total < 60 {
        return false;
    }
    let markers = text
        .chars()
        .filter(|c| MOJIBAKE_MARKERS.contains(*c))
        .count();
    markers * 5 >= total
}

/// 从乱码文本里取一段样例，贴进 issue 里让作者一眼认出是这份文件。
pub fn sample(text: &str, limit: usize) -> String {
    let line = text
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("");
    let cut: String = line.chars().take(limit).collect();
    if line.chars().count() > limit {
        format!("{cut}…")
    } else {
        cut
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 真实损坏文件的头几行（第 037 章，2026-11-10 事故现场）。
    const CORRUPTED: &str = "# 绗?37绔?琛ヤ綅浜轰笉琛?\n\
        绗簩鍗佷節澶╂棭涓婂叓鐐癸紝澶╄繕鏄槾鐨勩€備簯涓嶅啀寰€涓嬪帇锛屽彧鏄憡骞充簡锛屾暣鏉¤娌℃湁涓€澶勪寒涔熸病鏈変竴澶勬殫锛岄鑹插儚鏀炬棫浜嗙殑鎶ョ焊銆?\n\
        鍥存尅鏄ㄥぉ骞蹭簡锛岄搧鐨紳閲屽彂鍑哄緢缁嗙殑鍝ㄩ煶銆傞檲鐮氫粠鍝ㄩ煶搴曚笅璧拌繃鍘伙紝杩涗簡澶у爞銆?";

    /// 正常正文（同一章修好之后的开头）。
    const CLEAN: &str = "# 第037章 补位人不补\n\n\
        第二十九天早上八点，天还是阴的。云不再往下压，只是摊平了，整条街没有一处亮也没有一处暗，\
        颜色像放旧了的报纸。\n\n\
        围挡昨天干了，铁皮缝里发出很细的哨音。陈砚从哨音底下走过去，进了大堂。";

    #[test]
    fn real_corrupted_chapter_is_flagged() {
        assert!(looks_like_mojibake(CORRUPTED));
    }

    #[test]
    fn a_repaired_chapter_is_not_flagged() {
        assert!(!looks_like_mojibake(CLEAN));
    }

    #[test]
    fn short_texts_are_never_judged() {
        assert!(!looks_like_mojibake("绗?37绔?"));
    }

    #[test]
    fn private_use_characters_alone_are_enough() {
        let mut text = "他说这话的时候，屋里很静，窗外有一点雨声，落在铁皮上很轻。".to_string();
        text.push('\u{e0ff}');
        assert!(looks_like_mojibake(&text));
    }

    #[test]
    fn sample_takes_the_first_non_empty_line() {
        assert_eq!(sample("\n\n# 绗?37绔?\n正文", 12), "# 绗?37绔?");
        assert_eq!(sample("\n\n# 第037章 补位人不补\n正文", 6), "# 第037…");
    }
}
