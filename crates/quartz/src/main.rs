//! Quartz 可执行文件入口，负责核心装配、Windows 平台集成、存储、查询、托盘与浏览器 host。
//!
//! 核心模式默认常驻，负责采集与管道服务；界面模式按需启动 GPUI 窗口；host 模式由浏览器启动，在标准输入输出与命名管道之间转发消息。

// TODO(M0): 验证浏览器标准句柄继承和命令行控制台体验后，启用 Windows 子系统。
// #![windows_subsystem = "windows"]

fn main() {}
