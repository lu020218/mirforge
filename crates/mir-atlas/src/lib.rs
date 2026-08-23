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
    /// 放入一帧 RGBA，返回放置位置；超过页尺寸的帧返回 None。
    pub fn insert(&mut self, w: u32, h: u32, rgba: &[u8]) -> Option<Placed> {
        if w == 0 || h == 0 || rgba.len() < (w * h * 4) as usize {
            return None;
        }
        // 先试已有页 (末页优先, 早期页很快填满)
        for pi in (0..self.pages.len()).rev() {
            if let Some((x, y)) = self.pages[pi].packer.alloc(w, h) {
                self.blit(pi, x, y, w, h, rgba);
                return Some(Placed { page: pi, x, y });
            }
        }
        let mut page = PageBuf::default();
        let (x, y) = page.packer.alloc(w, h)?;
        self.pages.push(page);
        let pi = self.pages.len() - 1;
        self.blit(pi, x, y, w, h, rgba);
        Some(Placed { page: pi, x, y })
    }

    fn blit(&mut self, page: usize, x: u32, y: u32, w: u32, h: u32, rgba: &[u8]) {
        let p = &mut self.pages[page];
        for row in 0..h {
            let src = (row * w * 4) as usize;
            let dst = (((y + row) * PAGE_SIZE + x) * 4) as usize;
            p.rgba[dst..dst + (w * 4) as usize].copy_from_slice(&rgba[src..src + (w * 4) as usize]);
        }
        p.dirty = true;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pack_and_overflow_to_new_page() {
        let mut a = AtlasCpu::default();
        let big = vec![255u8; (1024 * 1024 * 4) as usize];
        let mut pages_seen = 0;
        for _ in 0..5 {
            let p = a.insert(1024, 1024, &big).unwrap();
            pages_seen = pages_seen.max(p.page + 1);
        }
        assert_eq!(pages_seen, 2); // 4 张/页 → 第 5 张进第 2 页
        assert!(a.pages[0].dirty);
    }

    #[test]
    fn shelf_reuse_and_pixel_blit() {
        let mut a = AtlasCpu::default();
        let px = |c: u8, n: usize| vec![c; n * 4];
        let p1 = a.insert(100, 30, &px(1, 3000)).unwrap();
        let p2 = a.insert(200, 30, &px(2, 6000)).unwrap();
        assert_eq!((p1.x, p1.y), (0, 0));
        assert_eq!((p2.x, p2.y), (100, 0)); // 同货架续排
        let p3 = a.insert(50, 64, &px(3, 3200)).unwrap();
        assert_eq!(p3.y, 30); // 新货架
                              // 像素落位
        let idx = ((p2.y * PAGE_SIZE + p2.x) * 4) as usize;
        assert_eq!(a.pages[0].rgba[idx], 2);
    }

    #[test]
    fn reject_oversize() {
        let mut a = AtlasCpu::default();
        assert!(a.insert(4096, 10, &vec![0; 4096 * 10 * 4]).is_none());
        assert!(a.insert(0, 10, &[]).is_none());
    }
}
