//! 配置存取已下沉 crates/gamedata (hub 与区服共用同一套建表/迁移/读写);
//! 此处二次导出保持站内 `crate::config_store::X` 路径与改造前一致。

pub use gamedata::store::*;
