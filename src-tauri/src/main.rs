// 无条件声明为窗口程序：debug 构建也不再附带黑色控制台（用户双击 exe 时弹出的
// 那个 cmd 就是它）。调试日志改用前端 toast / 文件，不依赖控制台。
#![windows_subsystem = "windows"]

fn main() {
    storydesk_lib::run()
}
