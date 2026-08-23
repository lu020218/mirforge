//! 对拍工具: 打印一行格子的 back/front 索引
//! 用法: cargo run -p mir-formats --example dumprow -- <map> <y> <x0> <x1>

fn main() {
    let a: Vec<String> = std::env::args().collect();
    let data = std::fs::read(&a[1]).unwrap();
    let m = mir_formats::map::parse(&data).unwrap();
    let y: u32 = a[2].parse().unwrap();
    let (x0, x1): (u32, u32) = (a[3].parse().unwrap(), a[4].parse().unwrap());
    let backs: Vec<String> = (x0..=x1)
        .map(|x| m.cell(x, y).unwrap().back.to_string())
        .collect();
    let fronts: Vec<String> = (x0..=x1)
        .map(|x| {
            let c = m.cell(x, y).unwrap();
            format!("{}@{}|m{}@{}", c.front, c.front_lib, c.mid, c.mid_lib)
        })
        .collect();
    println!("back  y={y}: {}", backs.join(","));
    println!("front y={y}: {}", fronts.join(","));
}
