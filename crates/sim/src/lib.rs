//! # sim
//!
//! 双端共享的移动/碰撞判定（服务器校验与客户端预测调用同一实现）。
//! 零重依赖。坐标单位 = 格（世界空间正方形格；屏幕投影 48×32 与此无关）。
//!
//! 精度体系（见 docs/ENGINE_REBOOT.md）：
//! - 行走性存 1/4 格子网格（每格 4×4 子格，12×8px）；
//!   原版地图阻挡是格级的，展开即可；将来精细脚印可直接写子格。
//! - 实体为圆形碰撞体（半径 [`BODY_RADIUS`] 格），圆覆盖的子格全可走才通过；
//! - 移动被挡时沿墙滑行（全向量 → 仅 x → 仅 y）。

/// 实体圆形碰撞体半径（格）
pub const BODY_RADIUS: f64 = 0.35;

/// NPC 可交互半径（格）
///
/// 两端共用：客户端据此决定"站定开口"还是"先走过去"，服务端据此拒绝越距对话。
/// 分别写两份常量迟早会漂移，故放在这里。
///
/// **只算直线距离，不做视线判定** —— 隔着房屋、围墙照样能对话（人物站在药店
/// 后面点得到老板）。这是刻意的：原版就没有遮挡限制，加了反而别扭。半径取 8 格
/// 是为了覆盖"绕到建筑背面"这类常见站位，同时仍留一个上界让服务端能挡掉越权。
pub const NPC_TALK_RANGE: f64 = 8.0;

/// 传奇 8 方向单位向量（Mir 枚举顺序：0=上，顺时针；屏幕 y 向下为正）
pub const DIR8: [(f64, f64); 8] = [
    (0.0, -1.0), // 0 上
    (
        std::f64::consts::FRAC_1_SQRT_2,
        -std::f64::consts::FRAC_1_SQRT_2,
    ), // 1 右上
    (1.0, 0.0),  // 2 右
    (
        std::f64::consts::FRAC_1_SQRT_2,
        std::f64::consts::FRAC_1_SQRT_2,
    ), // 3 右下
    (0.0, 1.0),  // 4 下
    (
        -std::f64::consts::FRAC_1_SQRT_2,
        std::f64::consts::FRAC_1_SQRT_2,
    ), // 5 左下
    (-1.0, 0.0), // 6 左
    (
        -std::f64::consts::FRAC_1_SQRT_2,
        -std::f64::consts::FRAC_1_SQRT_2,
    ), // 7 左上
];

/// 任意方向向量 → 最近的 Mir 8 向枚举（0=上, 顺时针）
pub fn dir8_from(dx: f64, dy: f64) -> usize {
    if dx == 0.0 && dy == 0.0 {
        return 4; // 缺省朝下 (面向镜头)
    }
    let ang = dx.atan2(-dy); // 0 = 上, 顺时针为正
    let sector = (ang / (std::f64::consts::PI / 4.0)).round() as i64;
    sector.rem_euclid(8) as usize
}

/// 行走性网格（1/4 格子精度）
#[derive(Debug, Clone)]
pub struct WalkGrid {
    /// 格宽/高
    pub width: u32,
    pub height: u32,
    /// 子格行走性，长度 = (4w)*(4h)，行主序，true=可走
    sub: Vec<bool>,
}

impl WalkGrid {
    /// 由格级阻挡构建（原版地图路径）：blocked(x,y) = 该格是否阻挡
    pub fn from_cells<F: Fn(u32, u32) -> bool>(width: u32, height: u32, blocked: F) -> Self {
        let (fw, fh) = ((width * 4) as usize, (height * 4) as usize);
        let mut sub = vec![false; fw * fh];
        for y in 0..height {
            for x in 0..width {
                if blocked(x, y) {
                    continue;
                }
                for sy in 0..4 {
                    for sx in 0..4 {
                        sub[(y as usize * 4 + sy) * fw + x as usize * 4 + sx] = true;
                    }
                }
            }
        }
        Self { width, height, sub }
    }

    #[inline]
    fn sub_at(&self, sx: i64, sy: i64) -> bool {
        if sx < 0 || sy < 0 || sx >= (self.width * 4) as i64 || sy >= (self.height * 4) as i64 {
            return false;
        }
        self.sub[sy as usize * (self.width * 4) as usize + sx as usize]
    }

    /// 直接写一个子格（精细脚印用）
    pub fn set_sub(&mut self, sx: u32, sy: u32, walkable: bool) {
        if sx < self.width * 4 && sy < self.height * 4 {
            let fw = (self.width * 4) as usize;
            self.sub[sy as usize * fw + sx as usize] = walkable;
        }
    }

    /// 圆形碰撞体判定：圆心 (x,y) 半径 r（格），圆覆盖的子格全可走才通过。
    /// 越界视为不可走。
    pub fn is_walkable_circle(&self, x: f64, y: f64, r: f64) -> bool {
        let x0 = ((x - r) * 4.0).floor() as i64;
        let x1 = ((x + r) * 4.0).floor() as i64;
        let y0 = ((y - r) * 4.0).floor() as i64;
        let y1 = ((y + r) * 4.0).floor() as i64;
        for sy in y0..=y1 {
            for sx in x0..=x1 {
                // 子格中心在圆内才参与判定 (标准近似)
                let cx = (sx as f64 + 0.5) / 4.0;
                let cy = (sy as f64 + 0.5) / 4.0;
                let (dx, dy) = (cx - x, cy - y);
                if dx * dx + dy * dy > r * r {
                    continue;
                }
                if !self.sub_at(sx, sy) {
                    return false;
                }
            }
        }
        true
    }

    /// 移动一步：目标不可达时沿墙滑行（全向量 → 仅 x → 仅 y → 原地）。
    /// 返回实际到达位置。
    pub fn try_move(&self, x: f64, y: f64, dx: f64, dy: f64, r: f64) -> (f64, f64) {
        let (nx, ny) = (x + dx, y + dy);
        if self.is_walkable_circle(nx, ny, r) {
            return (nx, ny);
        }
        if dx != 0.0 && self.is_walkable_circle(nx, y, r) {
            return (nx, y);
        }
        if dy != 0.0 && self.is_walkable_circle(x, ny, r) {
            return (x, ny);
        }
        (x, y)
    }
}

// ─────────── 网格寻路 (A*) ───────────

/// 单次寻路允许展开的最大节点数
///
/// 700x700 的图满打满算 49 万格; 正常寻路远到不了这个量, 设上限只是防着
/// "目标被墙围死" 这类退化情形把一帧卡住。
pub const PATH_BUDGET: usize = 60_000;

/// 八方向格位移与代价 (对角为 √2)
const STEP: [(i32, i32, f64); 8] = [
    (0, -1, 1.0),
    (1, -1, std::f64::consts::SQRT_2),
    (1, 0, 1.0),
    (1, 1, std::f64::consts::SQRT_2),
    (0, 1, 1.0),
    (-1, 1, std::f64::consts::SQRT_2),
    (-1, 0, 1.0),
    (-1, -1, std::f64::consts::SQRT_2),
];

#[derive(PartialEq)]
struct Node {
    f: f64,
    idx: u32,
}
impl Eq for Node {}
impl Ord for Node {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // BinaryHeap 是大顶堆, 这里反过来当小顶堆用
        other
            .f
            .partial_cmp(&self.f)
            .unwrap_or(std::cmp::Ordering::Equal)
    }
}
impl PartialOrd for Node {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// 八方向 A*: 从 `from` 走到 `to`, 返回途经格心 (含终点, 不含起点)
///
/// - 可走判定用 `is_walkable_circle`, 所以路径已经把身位半径算进去了;
/// - 对角要求两侧正交格都可走, 不允许贴着墙角斜穿;
/// - 目标不可站立时自动就近找一个可站的替代点 (点到墙上也能走到墙边);
/// - 找不到路或超出 [`PATH_BUDGET`] 返回 None。
pub fn find_path(
    grid: &WalkGrid,
    from: (f64, f64),
    to: (f64, f64),
    radius: f64,
) -> Option<Vec<(f64, f64)>> {
    let (w, h) = (grid.width as i32, grid.height as i32);
    let clampc = |v: f64, hi: i32| (v.floor() as i32).clamp(0, hi - 1);
    let (sx, sy) = (clampc(from.0, w), clampc(from.1, h));
    let (mut gx, mut gy) = (clampc(to.0, w), clampc(to.1, h));
    let ok = |x: i32, y: i32| {
        x >= 0
            && y >= 0
            && x < w
            && y < h
            && grid.is_walkable_circle(x as f64 + 0.5, y as f64 + 0.5, radius)
    };
    if (sx, sy) == (gx, gy) {
        return Some(Vec::new());
    }
    // 目标落在墙里: 向外螺旋找最近的可站格, 免得点到建筑上就完全没反应
    if !ok(gx, gy) {
        let mut found = None;
        'outer: for r in 1..=24i32 {
            for dy in -r..=r {
                for dx in -r..=r {
                    if dx.abs() != r && dy.abs() != r {
                        continue; // 只看这一圈的边
                    }
                    if ok(gx + dx, gy + dy) {
                        found = Some((gx + dx, gy + dy));
                        break 'outer;
                    }
                }
            }
        }
        let (nx, ny) = found?;
        gx = nx;
        gy = ny;
    }
    if !ok(sx, sy) {
        return None; // 自己都卡在墙里, 交给服务端的落点吸附去处理
    }

    let n = (w * h) as usize;
    let idx = |x: i32, y: i32| (y * w + x) as u32;
    let mut g = vec![f64::INFINITY; n];
    let mut came: Vec<u32> = vec![u32::MAX; n];
    let mut closed = vec![false; n];
    let octile = |x: i32, y: i32| {
        let (dx, dy) = (((x - gx).abs()) as f64, ((y - gy).abs()) as f64);
        let (lo, hi) = if dx < dy { (dx, dy) } else { (dy, dx) };
        hi - lo + std::f64::consts::SQRT_2 * lo
    };
    let mut heap = std::collections::BinaryHeap::new();
    g[idx(sx, sy) as usize] = 0.0;
    heap.push(Node {
        f: octile(sx, sy),
        idx: idx(sx, sy),
    });
    let mut expanded = 0usize;
    while let Some(Node { idx: cur, .. }) = heap.pop() {
        if closed[cur as usize] {
            continue;
        }
        closed[cur as usize] = true;
        let (cx, cy) = ((cur as i32) % w, (cur as i32) / w);
        if (cx, cy) == (gx, gy) {
            // 回溯并抽稀: 只保留拐点, 直线段中间的格不必逐个走
            let mut cells = Vec::new();
            let mut p = cur;
            while p != idx(sx, sy) {
                cells.push(((p as i32) % w, (p as i32) / w));
                p = came[p as usize];
            }
            cells.reverse();
            let mut out: Vec<(f64, f64)> = Vec::with_capacity(cells.len());
            for i in 0..cells.len() {
                let keep = i + 1 == cells.len() || {
                    let prev = if i == 0 { (sx, sy) } else { cells[i - 1] };
                    let (a, b) = (cells[i], cells[i + 1]);
                    (a.0 - prev.0, a.1 - prev.1) != (b.0 - a.0, b.1 - a.1)
                };
                if keep {
                    out.push((cells[i].0 as f64 + 0.5, cells[i].1 as f64 + 0.5));
                }
            }
            return Some(out);
        }
        expanded += 1;
        if expanded > PATH_BUDGET {
            return None;
        }
        for (dx, dy, cost) in STEP {
            let (nx, ny) = (cx + dx, cy + dy);
            if !ok(nx, ny) {
                continue;
            }
            // 不许贴着墙角斜穿
            if dx != 0 && dy != 0 && (!ok(cx + dx, cy) || !ok(cx, cy + dy)) {
                continue;
            }
            let ni = idx(nx, ny);
            let ng = g[cur as usize] + cost;
            if ng + 1e-9 < g[ni as usize] {
                g[ni as usize] = ng;
                came[ni as usize] = cur;
                heap.push(Node {
                    f: ng + octile(nx, ny),
                    idx: ni,
                });
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 空旷 20×20; 便于构造带墙的用例
    fn open(w: u32, h: u32, blocked: &[(u32, u32)]) -> WalkGrid {
        WalkGrid::from_cells(w, h, |x, y| blocked.contains(&(x, y)))
    }

    #[test]
    fn path_straight_line_is_one_waypoint() {
        let g = open(20, 20, &[]);
        // 直线段被抽稀成一个拐点(即终点)
        let p = find_path(&g, (2.5, 2.5), (9.5, 2.5), BODY_RADIUS).unwrap();
        assert_eq!(p.len(), 1, "直线不该留中间点: {p:?}");
        assert_eq!(p[0], (9.5, 2.5));
    }

    #[test]
    fn path_goes_around_wall() {
        // 竖墙横在中间, 只在 y=0 处留口
        let wall: Vec<(u32, u32)> = (1..20).map(|y| (10u32, y)).collect();
        let g = open(20, 20, &wall);
        let p = find_path(&g, (5.5, 10.5), (15.5, 10.5), BODY_RADIUS).expect("应能绕行");
        // 终点对; 且路径确实绕到了缺口附近 (最小 y 明显小于起点)
        assert_eq!(*p.last().unwrap(), (15.5, 10.5));
        let min_y = p.iter().map(|w| w.1).fold(f64::INFINITY, f64::min);
        assert!(min_y < 2.0, "应从上方缺口绕行, 实际最小 y={min_y}");
    }

    #[test]
    fn path_none_when_fully_walled_off() {
        // 把目标格四周全封死
        let wall = [
            (14, 13),
            (15, 13),
            (16, 13),
            (14, 14),
            (16, 14),
            (14, 15),
            (15, 15),
            (16, 15),
        ];
        let g = open(20, 20, &wall);
        assert!(find_path(&g, (2.5, 2.5), (15.5, 14.5), BODY_RADIUS).is_none());
    }

    #[test]
    fn path_snaps_goal_out_of_wall() {
        // 点在墙上: 就近落到可站格, 而不是直接失败
        let g = open(20, 20, &[(10, 10)]);
        let p = find_path(&g, (2.5, 2.5), (10.5, 10.5), BODY_RADIUS).expect("应吸附到墙边");
        assert_ne!(*p.last().unwrap(), (10.5, 10.5));
    }

    #[test]
    fn path_same_cell_is_empty() {
        let g = open(20, 20, &[]);
        assert_eq!(
            find_path(&g, (5.2, 5.9), (5.8, 5.1), BODY_RADIUS)
                .unwrap()
                .len(),
            0
        );
    }

    #[test]
    fn path_does_not_cut_corners() {
        // 对角两侧各有一堵, 不该斜着穿过去
        let g = open(20, 20, &[(6, 5), (5, 6)]);
        let p = find_path(&g, (5.5, 5.5), (6.5, 6.5), BODY_RADIUS);
        // 要么绕开(多于 1 个拐点), 要么无路; 总之不能一步斜穿
        if let Some(p) = &p {
            assert!(p.len() > 1, "不该贴角斜穿: {p:?}");
        }
    }

    /// 3×3, 中心格阻挡
    fn grid() -> WalkGrid {
        WalkGrid::from_cells(3, 3, |x, y| x == 1 && y == 1)
    }

    #[test]
    fn circle_blocking() {
        let g = grid();
        assert!(g.is_walkable_circle(0.5, 0.5, BODY_RADIUS));
        assert!(!g.is_walkable_circle(1.5, 1.5, BODY_RADIUS)); // 中心格内
                                                               // 紧贴阻挡格边缘: 圆心离格边 > r 才可走
        assert!(g.is_walkable_circle(0.5, 1.5, BODY_RADIUS));
        assert!(!g.is_walkable_circle(0.95, 1.5, BODY_RADIUS)); // 圆侵入阻挡格
                                                                // 地图边界外不可走
        assert!(!g.is_walkable_circle(-0.2, 0.5, BODY_RADIUS));
    }

    #[test]
    fn slide_along_wall() {
        let g = grid();
        // 从阻挡格左侧向右下走: x 被挡, 沿 y 滑
        let (nx, ny) = g.try_move(0.5, 1.5, 0.4, 0.3, BODY_RADIUS);
        assert_eq!(nx, 0.5);
        assert!((ny - 1.8).abs() < 1e-9);
        // 开阔地全向量直通
        let (nx, ny) = g.try_move(0.5, 0.5, 0.3, 0.0, BODY_RADIUS);
        assert!((nx - 0.8).abs() < 1e-9 && ny == 0.5);
        // 完全被挡原地不动 (贴左上角往阻挡格里挤)
        let (nx, ny) = g.try_move(0.5, 0.5, -0.5, -0.5, BODY_RADIUS);
        // 边界外不可走 → x/y 单轴也不行
        assert_eq!((nx, ny), (0.5, 0.5));
    }

    #[test]
    fn dir8_sectors() {
        assert_eq!(dir8_from(0.0, -1.0), 0); // 上
        assert_eq!(dir8_from(1.0, -1.0), 1); // 右上
        assert_eq!(dir8_from(1.0, 0.0), 2); // 右
        assert_eq!(dir8_from(0.0, 1.0), 4); // 下
        assert_eq!(dir8_from(-1.0, 0.0), 6); // 左
        assert_eq!(dir8_from(-1.0, -1.0), 7); // 左上
    }

    #[test]
    fn set_sub_fine_footprint() {
        let mut g = WalkGrid::from_cells(2, 1, |_, _| false);
        g.set_sub(3, 1, false); // 封一个子格
        assert!(!g.is_walkable_circle(0.9, 0.4, 0.2));
        assert!(g.is_walkable_circle(0.4, 0.4, 0.2));
    }
}
