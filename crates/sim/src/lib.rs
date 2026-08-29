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
pub const NPC_TALK_RANGE: f64 = 3.0;

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

#[cfg(test)]
mod tests {
    use super::*;

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
