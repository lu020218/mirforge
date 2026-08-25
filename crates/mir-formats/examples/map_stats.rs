//! 调试工具: 统计 .map 各层库号引用分布与动画/门格数量。
//! 用法: cargo run -p mir-formats --example map_stats -- <map路径>

use std::collections::BTreeMap;

fn main() {
    let path = std::env::args().nth(1).expect("用法: map_stats <map路径>");
    let map = mir_formats::map::parse(&std::fs::read(&path).expect("读取失败")).expect("解析失败");
    println!("{:?} {}x{}", map.kind, map.width, map.height);
    let (mut back, mut mid, mut front) = (BTreeMap::new(), BTreeMap::new(), BTreeMap::new());
    let (mut ani, mut blend, mut doors) = (0u32, 0u32, 0u32);
    for c in &map.cells {
        if c.back >= 0 {
            *back.entry(c.back_lib).or_insert(0u32) += 1;
        }
        if c.mid >= 0 {
            *mid.entry(c.mid_lib).or_insert(0u32) += 1;
        }
        if c.front >= 0 {
            *front.entry(c.front_lib).or_insert(0u32) += 1;
        }
        if c.ani_frame & 0x7F > 0 {
            ani += 1;
        }
        if c.ani_frame & 0x80 > 0 {
            blend += 1;
        }
        if c.door_index > 0 {
            doors += 1;
        }
    }
    println!("back  libs: {back:?}");
    println!("mid   libs: {mid:?}");
    println!("front libs: {front:?}");
    println!("动画格 {ani}, blend 格 {blend}, 门格 {doors}");
}
