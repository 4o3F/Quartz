//! 计时状态机、区间运算、可用状态裁剪、规则匹配、提取与隐私策略求值的纯逻辑；时间值由外壳作为输入传入，不依赖 Windows API、SQLite 或 GPUI，可在任意平台测试。

pub mod capture;
pub mod domain_name;
pub mod facts;
pub mod text;

pub mod rules;
