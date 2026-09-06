//! 双端共享的资源布局契约(帧表/段位/弹速)。
//!
//! 这些常量是客户端渲染与服务端结算必须一致的"格式约定"——此前散落在
//! 两端代码注释里,施法段基址错位、飞行方向两次返工皆源于此,故收敛至此
//! 由编译器守约。数值全部来自对购买素材的逐槽实测(见各项注释)。

/// 人物帧表(衣甲与武器库同布局;实测自购买素材裸模全库分段)。
/// 每方向 8 帧位,8 方向 0=上顺时针;动作基址 + 方向×8 寻址。
pub mod hum {
    /// 方向块跨度(帧位)
    pub const DIR_STRIDE: usize = 8;
    /// 站立(4 帧)
    pub const STAND: usize = 0;
    /// 行走(6 帧)
    pub const WALK: usize = 64;
    /// 跑步(6 帧)
    pub const RUN: usize = 128;
    /// 攻击/挥砍(6 帧)
    pub const ATTACK: usize = 192;
    /// 施法/双手前推(6 帧)。实测起于 392 —— 384 是前一个 8 帧旋身
    /// 动作的末块,中段布局并非齐整 64 槽对齐,不可按整块推算
    pub const CAST: usize = 392;
    /// 死亡(4 帧)
    pub const DIE: usize = 536;
    /// 女版整段偏移
    pub const FEMALE: usize = 600;
    /// 立绘用朝南站立首帧(dir=4)
    pub const PORTRAIT_SOUTH: usize = STAND + 4 * DIR_STRIDE;
}

/// 单技能特效标准文件布局(packs/magic/100+,统一 200 帧位)。
/// 段位置是格式约定,新素材按此模板打包即可直接三段。
pub mod fx {
    /// 起手段基址(≤10 帧,播于施放者脚下)
    pub const CAST: i32 = 0;
    /// 飞行段基址(16 向 × 每向 [`SLOT`] 槽)
    pub const FLY: i32 = 10;
    /// 每块槽数(起手块与每个飞行方向行均为 10 槽,实帧数逐块实测)
    pub const SLOT: i32 = 10;
    /// 命中段基址(≤30 帧)
    pub const HIT: i32 = 170;
    /// 统一帧位总数
    pub const TOTAL: usize = 200;
    /// 弹体飞行速度(格/秒),服务端延迟结算与客户端弹体动画共用
    pub const FLY_SPEED: f64 = 14.0;

    /// 飞行方向 → 段内行号。
    ///
    /// 素材 16 向行序为**逆时针**,头向 ≈ 197.5° − 22.5°×行号
    /// (对全部 16 行做亮度质心测量拟合:行1=下、行5=右、行9=上、行13=左)。
    /// 入参为世界格位移(屏幕坐标系,y 向下为正)。
    pub fn fly_row(dx: f64, dy: f64) -> i32 {
        // 0° = 上,顺时针
        let deg = dx.atan2(-dy).to_degrees().rem_euclid(360.0);
        (((197.5 - deg) / 22.5).round() as i32).rem_euclid(16)
    }

    /// 某方向行的帧基址
    pub fn fly_base(row: i32) -> i32 {
        FLY + row * SLOT
    }

    /// 特效名 → packs/magic 下文件主名。
    /// 特效以**技能英文名**为 id (如 "zhiyu" → magic/zhiyu.mfl, 与技能 id
    /// 一致, 管理台配置一眼对应); 纯数字视为旧编号库 (如 "6" → magic/006.mfl,
    /// 素材源合集仍可引用)。两端共用此规则。
    pub fn file_stem(fx: &str) -> String {
        if !fx.is_empty() && fx.bytes().all(|b| b.is_ascii_digit()) {
            format!("{:03}", fx.parse::<u32>().unwrap_or(0))
        } else {
            fx.to_string()
        }
    }
}

/// 怪物库自适应帧表(纯算法;帧存在性经 `real` 回调注入,便于表驱动测试)。
/// 市售怪物库每库基址/跨度不一且一库多怪,寻址错段的历史事故
/// (鸡死变鹿)由"段尾栅栏"拦截 —— 算法契约见各函数注释。
pub mod mon {
    /// 一只怪在库内的段元数据
    #[derive(Clone, Copy, PartialEq, Eq, Debug)]
    pub struct Meta {
        /// 段起始帧(首实帧)
        pub base: i32,
        /// 方向块跨度
        pub stride: i32,
        /// 段尾栅栏:首个 ≥ 跨度的空洞处 —— 越过它就是同库下一只怪
        pub end: i32,
    }

    /// 自配置基址探测一只怪的段:首实帧为 base,块内实帧连跑 + 下一块
    /// 起点推出 stride,首个 ≥ stride 的空洞定 end(上限 8 动作 × 8 向)。
    pub fn detect(mut real: impl FnMut(i32) -> bool, cfg_base: i32) -> Option<Meta> {
        let start = cfg_base;
        let base = (start..start + 4000).find(|&i| real(i))?;
        let run = (1..64).find(|&d| !real(base + d)).unwrap_or(64);
        let stride = (run..64)
            .find(|&d| real(base + d))
            .unwrap_or(10)
            .clamp(run, 32);
        let cap = base + stride * 8 * 8;
        let mut end = cap;
        let mut gap = 0;
        let mut i = base;
        while i < cap {
            if real(i) {
                gap = 0;
            } else {
                gap += 1;
                if gap >= stride {
                    end = i - gap + 1;
                    break;
                }
            }
            i += 1;
        }
        Some(Meta { base, stride, end })
    }

    /// 方向块内连续实帧数(≤ stride)。动画相位对它取模,不踩空帧位
    pub fn block_len(mut real: impl FnMut(i32) -> bool, base: i32, stride: i32) -> u8 {
        let mut k = 0u8;
        for i in 0..stride {
            if real(base + i) {
                k += 1;
            } else {
                break;
            }
        }
        k
    }
}

#[cfg(test)]
mod tests {
    use super::fx;

    /// 四个正方向必须落在实测确认的行上(拟合锚点)
    #[test]
    fn fly_row_cardinals() {
        assert_eq!(fx::fly_row(0.0, 1.0), 1, "向下 = 行1");
        assert_eq!(fx::fly_row(1.0, 0.0), 5, "向右 = 行5");
        assert_eq!(fx::fly_row(0.0, -1.0), 9, "向上 = 行9");
        assert_eq!(fx::fly_row(-1.0, 0.0), 13, "向左 = 行13");
    }

    /// 对照亮度质心逐行实测的头向角度表:朝该角度发射时选中的行
    /// 与实测行号至多差 1(个别行素材头向偏离拟合线 ~12°,属绘制误差)。
    /// 数据来源:packs/magic/100 火球飞行段,每行取 3 帧质心平均。
    #[test]
    fn fly_row_matches_measured_heads() {
        let measured: [(i32, f64); 14] = [
            (1, 174.8),
            (2, 149.9),
            (3, 126.9),
            (5, 82.8),
            (6, 65.9),
            (7, 40.4),
            (8, 30.0),
            (9, 347.8),
            (10, 328.3),
            (11, 317.4),
            (12, 285.3),
            (13, 259.0),
            (14, 245.2),
            (15, 218.9),
        ];
        for (row, head_deg) in measured {
            let rad = head_deg.to_radians();
            // 头向角度 → 单位位移 (0°=上, 顺时针, 屏幕 y 向下)
            let (dx, dy) = (rad.sin(), -rad.cos());
            let got = fx::fly_row(dx, dy);
            let diff = (got - row).rem_euclid(16).min((row - got).rem_euclid(16));
            assert!(diff <= 1, "头向 {head_deg}° 应选行 {row}±1, 实选 {got}");
        }
    }

    use super::mon;

    /// 合成一库两只怪: 甲 0..348 (10 跨度块, 每块 6 实帧), 空洞后乙自 360 起。
    /// 这是"鸡死变鹿"的回归样例: 段尾栅栏必须落在空洞处, 绝不能到 360
    #[test]
    fn mon_detect_fences_entity_end() {
        let real = |i: i32| ((0..348).contains(&i) || (360..600).contains(&i)) && i % 10 < 6;
        let a = mon::detect(real, 0).unwrap();
        assert_eq!((a.base, a.stride), (0, 10));
        assert!(a.end <= 348, "甲的段尾 {} 不得越进乙 (360 起)", a.end);
        // 死亡块 (动作 4) 仍在甲段内
        assert!(a.base + 4 * a.stride * 8 < a.end);
        let b = mon::detect(real, 360).unwrap();
        assert_eq!((b.base, b.stride), (360, 10));
    }

    /// 基址偏移的库 (Mon7 式: 前 440 帧全空)
    #[test]
    fn mon_detect_skips_leading_empties() {
        let real = |i: i32| (440..800).contains(&i) && i % 8 < 5;
        let m = mon::detect(real, 0).unwrap();
        assert_eq!((m.base, m.stride), (440, 8));
    }

    /// 空库 → None; 块实帧数按跨度截断
    #[test]
    fn mon_detect_empty_and_block_len() {
        assert!(mon::detect(|_| false, 0).is_none());
        let real = |i: i32| i % 10 < 3;
        assert_eq!(mon::block_len(real, 0, 10), 3);
        assert_eq!(mon::block_len(real, 3, 10), 0);
        assert_eq!(mon::block_len(|_| true, 0, 10), 10);
    }

    /// 特效名 → 文件主名: 名字原样, 纯数字补零成旧编号
    #[test]
    fn fx_file_stem_rule() {
        assert_eq!(fx::file_stem("zhiyu"), "zhiyu");
        assert_eq!(fx::file_stem("shidu_red"), "shidu_red");
        assert_eq!(fx::file_stem("6"), "006");
        assert_eq!(fx::file_stem("100"), "100");
        assert_eq!(fx::file_stem(""), "");
    }

    /// 段布局不重叠且在总帧位内 (const 断言, 编译期即验证)
    #[test]
    fn fx_layout_bounds() {
        const { assert!(fx::CAST + fx::SLOT <= fx::FLY) };
        const { assert!(fx::HIT < fx::TOTAL as i32) };
        assert_eq!(fx::fly_base(15) + fx::SLOT, fx::HIT);
    }
}
