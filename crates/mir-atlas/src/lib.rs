//! # mir-atlas
//!
//! 运行时纹理图集的 CPU 侧逻辑：货架式(shelf)打包 + 多页管理。
//! 渲染端(Bevy)持有与每页对应的 GPU 纹理，页标脏后重新上传。
//! 磁盘缓存(二启秒开)为 M1 后续任务，接口预留于此 crate。

/// 图集页边长（px）
pub const PAGE_SIZE: u32 = 2048;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Placed {
    pub page: usize,
    pub x: u32,
    pub y: u32,
}

/// 单页货架打包器
#[derive(Debug, Default)]
struct Packer {
    shelves: Vec<(u32, u32, u32)>, // (y, 高, 已用宽)
    next_y: u32,
}

impl Packer {
    fn alloc(&mut self, w: u32, h: u32) -> Option<(u32, u32)> {
        if w > PAGE_SIZE || h > PAGE_SIZE {
            return None;
        }
        // 找放得下且高度浪费最小的货架
        let mut best: Option<(usize, u32)> = None;
        for (i, &(_, sh, used)) in self.shelves.iter().enumerate() {
            if h <= sh && used + w <= PAGE_SIZE {
                let waste = sh - h;
                if best.is_none_or(|(_, bw)| waste < bw) {
                    best = Some((i, waste));
                }
            }
        }
        if let Some((i, _)) = best {
            let (y, _, used) = self.shelves[i];
            self.shelves[i].2 += w;
            return Some((used, y));
        }
        // 开新货架
        if self.next_y + h <= PAGE_SIZE {
            let y = self.next_y;
            self.next_y += h;
            self.shelves.push((y, h, w));
            return Some((0, y));
        }
        None
    }
}

/// 一页的 CPU 像素缓冲
pub struct PageBuf {
    pub rgba: Vec<u8>,
    pub dirty: bool,
    packer: Packer,
}

impl Default for PageBuf {
    fn default() -> Self {
        Self {
            rgba: vec![0; (PAGE_SIZE * PAGE_SIZE * 4) as usize],
            dirty: false,
            packer: Packer::default(),
        }
    }
}

/// 多页图集（CPU 侧）
#[derive(Default)]
pub struct AtlasCpu {
    pub pages: Vec<PageBuf>,
}

impl AtlasCpu {
    /// 放入一帧 RGBA，返回放置位置（含 1px gutter：四周复制边缘像素，
    /// 防非整数缩放采样溢到相邻帧）；超过页尺寸的帧返回 None。
    pub fn insert(&mut self, w: u32, h: u32, rgba: &[u8]) -> Option<Placed> {
        if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
            return None;
        }
        let (pw, ph) = (w + 2, h + 2);
        // 先试已有页 (末页优先, 早期页很快填满)
        for pi in (0..self.pages.len()).rev() {
            if let Some((x, y)) = self.pages[pi].packer.alloc(pw, ph) {
                self.blit_padded(pi, x, y, w, h, rgba);
                return Some(Placed {
                    page: pi,
                    x: x + 1,
                    y: y + 1,
                });
            }
        }
        let mut page = PageBuf::default();
        let (x, y) = page.packer.alloc(pw, ph)?;
        self.pages.push(page);
        let pi = self.pages.len() - 1;
        self.blit_padded(pi, x, y, w, h, rgba);
        Some(Placed {
            page: pi,
            x: x + 1,
            y: y + 1,
        })
    }

    /// 页填充率 (货架已分配面积 / 页面积)
    pub fn fill_ratio(&self, page: usize) -> f32 {
        let Some(p) = self.pages.get(page) else {
            return 0.0;
        };
        let used: u32 = p.packer.shelves.iter().map(|&(_, h, w)| h * w).sum();
        used as f32 / (PAGE_SIZE * PAGE_SIZE) as f32
    }

    /// (x,y) 为含 gutter 的外框左上角; 图像画在 (x+1,y+1), 边框复制边缘像素
    fn blit_padded(&mut self, page: usize, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        let p = &mut self.pages[page];
        let row_bytes = (w * 4) as usize;
        for row in 0..h {
            let src = row as usize * row_bytes;
            let dst = (((y + 1 + row) * PAGE_SIZE + x + 1) * 4) as usize;
            p.rgba[dst..dst + row_bytes].copy_from_slice(&rgba[src..src + row_bytes]);
            // 左右 gutter 列 = 行首/行尾像素
            let dl = (((y + 1 + row) * PAGE_SIZE + x) * 4) as usize;
            let dr = (((y + 1 + row) * PAGE_SIZE + x + 1 + w) * 4) as usize;
            let (l, r) = (src, src + row_bytes - 4);
            p.rgba[dl..dl + 4].copy_from_slice(&rgba[l..l + 4]);
            p.rgba[dr..dr + 4].copy_from_slice(&rgba[r..r + 4]);
        }
        // 上下 gutter 行 = 首/末行(含左右角)整行复制
        let top_src = ((y + 1) * PAGE_SIZE + x) as usize * 4;
        let top_dst = (y * PAGE_SIZE + x) as usize * 4;
        let span = ((w + 2) * 4) as usize;
        p.rgba.copy_within(top_src..top_src + span, top_dst);
        let bot_src = ((y + h) * PAGE_SIZE + x) as usize * 4;
        let bot_dst = ((y + h + 1) * PAGE_SIZE + x) as usize * 4;
        p.rgba.copy_within(bot_src..bot_src + span, bot_dst);
        p.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_and_overflow_to_new_page() {
        let mut a = AtlasCpu::default();
        let big = vec![255u8; (1022 * 1022 * 4) as usize];
        let mut pages_seen = 0;
        for _ in 0..5 {
            let p = a.insert(1022, 1022, &big).unwrap(); // 含 gutter 1024²
            pages_seen = pages_seen.max(p.page + 1);
        }
        assert_eq!(pages_seen, 2); // 4 张/页 → 第 5 张进第 2 页
        assert!(a.pages[0].dirty);
        assert!(a.fill_ratio(0) > 0.99);
    }

    #[test]
    fn shelf_reuse_and_pixel_blit() {
        let mut a = AtlasCpu::default();
        let px = |c: u8, n: usize| vec![c; n * 4];
        let p1 = a.insert(100, 30, &px(1, 3000)).unwrap();
        let p2 = a.insert(200, 30, &px(2, 6000)).unwrap();
        assert_eq!((p1.x, p1.y), (1, 1)); // 内框 = 外框 +1 gutter
        assert_eq!((p2.x, p2.y), (103, 1)); // 同货架续排 (102 外框宽后)
        let p3 = a.insert(50, 64, &px(3, 3200)).unwrap();
        assert_eq!(p3.y, 33); // 新货架 y=32
                              // 像素落位
        let idx = ((p2.y * PAGE_SIZE + p2.x) * 4) as usize;
        assert_eq!(a.pages[0].rgba[idx], 2);
    }

    #[test]
    fn gutter_replicates_edges() {
        let mut a = AtlasCpu::default();
        // 2×2: 左上5 右上6 左下7 右下8
        let img = [5u8, 5, 5, 255, 6, 6, 6, 255, 7, 7, 7, 255, 8, 8, 8, 255];
        let p = a.insert(2, 2, &img).unwrap();
        let at = |x: u32, y: u32| a.pages[0].rgba[((y * PAGE_SIZE + x) * 4) as usize];
        assert_eq!(at(p.x, p.y), 5);
        // 四边 gutter = 邻接边像素
        assert_eq!(at(p.x - 1, p.y), 5); // 左
        assert_eq!(at(p.x + 2, p.y), 6); // 右
        assert_eq!(at(p.x, p.y - 1), 5); // 上
        assert_eq!(at(p.x, p.y + 2), 7); // 下
                                         // 四角
        assert_eq!(at(p.x - 1, p.y - 1), 5);
        assert_eq!(at(p.x + 2, p.y + 2), 8);
    }

    #[test]
    fn reject_oversize() {
        let mut a = AtlasCpu::default();
        assert!(a.insert(4096, 10, &vec![0; 4096 * 10 * 4]).is_none());
        // 含 gutter 后恰好超页宽也拒绝
        assert!(a.insert(2047, 10, &vec![0; 2047 * 10 * 4]).is_none());
        assert!(a.insert(0, 10, &[]).is_none());
    }
}
