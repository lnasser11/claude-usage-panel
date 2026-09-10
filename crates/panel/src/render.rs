//! Direct2D rendering of the panel into a 32-bit premultiplied DIB, which the
//! window hands to UpdateLayeredWindow. All coordinates are physical pixels;
//! `scale` converts the logical layout in `view.rs`.

use windows::{
    core::{w, Interface, Result},
    Win32::{
        Foundation::RECT,
        Graphics::{
            Direct2D::{
                Common::{D2D1_ALPHA_MODE_PREMULTIPLIED, D2D1_COLOR_F, D2D1_PIXEL_FORMAT, D2D_RECT_F},
                D2D1CreateFactory, ID2D1Brush, ID2D1DCRenderTarget, ID2D1Factory, ID2D1SolidColorBrush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED, D2D1_FEATURE_LEVEL_DEFAULT,
                D2D1_RENDER_TARGET_PROPERTIES, D2D1_RENDER_TARGET_TYPE_DEFAULT, D2D1_RENDER_TARGET_USAGE_NONE,
                D2D1_ROUNDED_RECT,
            },
            DirectWrite::{
                DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat, DWRITE_FACTORY_TYPE_SHARED,
                DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL, DWRITE_FONT_WEIGHT_NORMAL,
                DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER,
                DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING, DWRITE_WORD_WRAPPING_NO_WRAP,
            },
            Dxgi::Common::DXGI_FORMAT_B8G8R8A8_UNORM,
            Gdi::{
                CreateCompatibleDC, CreateDIBSection, DeleteDC, DeleteObject, SelectObject, BITMAPINFO,
                BITMAPINFOHEADER, BI_RGB, DIB_RGB_COLORS, HBITMAP, HDC, HGDIOBJ,
            },
        },
    },
};

use crate::view::{BarState, ViewModel, PAD, ROW_BAR, ROW_TEXT};

/// A top-down 32-bit DIB selected into a memory DC.
pub struct Frame {
    pub hdc: HDC,
    bitmap: HBITMAP,
    old: HGDIOBJ,
    pub width: i32,
    pub height: i32,
}

impl Frame {
    pub fn new(width: i32, height: i32) -> Result<Frame> {
        unsafe {
            let hdc = CreateCompatibleDC(None);
            let bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height,
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    ..Default::default()
                },
                ..Default::default()
            };
            let mut bits = std::ptr::null_mut();
            let bitmap = CreateDIBSection(Some(hdc), &bmi, DIB_RGB_COLORS, &mut bits, None, 0)?;
            let old = SelectObject(hdc, bitmap.into());
            Ok(Frame { hdc, bitmap, old, width, height })
        }
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe {
            SelectObject(self.hdc, self.old);
            let _ = DeleteObject(self.bitmap.into());
            let _ = DeleteDC(self.hdc);
        }
    }
}

struct Fonts {
    label: IDWriteTextFormat,
    label_right: IDWriteTextFormat,
    pct: IDWriteTextFormat,
    body: IDWriteTextFormat,
    body_right: IDWriteTextFormat,
    small: IDWriteTextFormat,
    header: IDWriteTextFormat,
}

pub struct Renderer {
    _d2d: ID2D1Factory,
    dwrite: IDWriteFactory,
    rt: ID2D1DCRenderTarget,
    brush: ID2D1SolidColorBrush,
    fonts: Option<(f32, Fonts)>,
}

fn rgba(r: u8, g: u8, b: u8, a: f32) -> D2D1_COLOR_F {
    D2D1_COLOR_F { r: r as f32 / 255.0, g: g as f32 / 255.0, b: b as f32 / 255.0, a }
}

const BG: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.102, g: 0.106, b: 0.125, a: 0.97 };
const TEXT: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.95, g: 0.95, b: 0.96, a: 1.0 };
const MUTED: D2D1_COLOR_F = D2D1_COLOR_F { r: 0.62, g: 0.63, b: 0.68, a: 1.0 };
const TRACK: D2D1_COLOR_F = D2D1_COLOR_F { r: 1.0, g: 1.0, b: 1.0, a: 0.10 };

impl Renderer {
    pub fn new() -> Result<Renderer> {
        unsafe {
            let d2d: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;
            let props = D2D1_RENDER_TARGET_PROPERTIES {
                r#type: D2D1_RENDER_TARGET_TYPE_DEFAULT,
                pixelFormat: D2D1_PIXEL_FORMAT { format: DXGI_FORMAT_B8G8R8A8_UNORM, alphaMode: D2D1_ALPHA_MODE_PREMULTIPLIED },
                dpiX: 96.0,
                dpiY: 96.0,
                usage: D2D1_RENDER_TARGET_USAGE_NONE,
                minLevel: D2D1_FEATURE_LEVEL_DEFAULT,
            };
            let rt = d2d.CreateDCRenderTarget(&props)?;
            let brush = rt.CreateSolidColorBrush(&TEXT, None)?;
            Ok(Renderer { _d2d: d2d, dwrite, rt, brush, fonts: None })
        }
    }

    fn font(&self, size: f32, weight: windows::Win32::Graphics::DirectWrite::DWRITE_FONT_WEIGHT, right: bool, vcenter: bool) -> Result<IDWriteTextFormat> {
        unsafe {
            let f = self.dwrite.CreateTextFormat(
                w!("Segoe UI"),
                None,
                weight,
                DWRITE_FONT_STYLE_NORMAL,
                DWRITE_FONT_STRETCH_NORMAL,
                size,
                w!("en-us"),
            )?;
            f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
            if right {
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_TRAILING)?;
            } else {
                f.SetTextAlignment(DWRITE_TEXT_ALIGNMENT_LEADING)?;
            }
            if vcenter {
                f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
            }
            Ok(f)
        }
    }

    fn fonts(&mut self, scale: f32) -> Result<()> {
        if let Some((s, _)) = &self.fonts {
            if (*s - scale).abs() < 0.001 {
                return Ok(());
            }
        }
        let fonts = Fonts {
            label: self.font(12.5 * scale, DWRITE_FONT_WEIGHT_SEMI_BOLD, false, false)?,
            label_right: self.font(11.5 * scale, DWRITE_FONT_WEIGHT_NORMAL, true, false)?,
            pct: self.font(13.0 * scale, DWRITE_FONT_WEIGHT_SEMI_BOLD, true, true)?,
            body: self.font(12.0 * scale, DWRITE_FONT_WEIGHT_NORMAL, false, true)?,
            body_right: self.font(11.5 * scale, DWRITE_FONT_WEIGHT_NORMAL, true, true)?,
            small: self.font(10.5 * scale, DWRITE_FONT_WEIGHT_NORMAL, false, true)?,
            header: self.font(11.0 * scale, DWRITE_FONT_WEIGHT_SEMI_BOLD, false, true)?,
        };
        self.fonts = Some((scale, fonts));
        Ok(())
    }

    fn text(&self, s: &str, f: &IDWriteTextFormat, rect: D2D_RECT_F, color: D2D1_COLOR_F) {
        let wide: Vec<u16> = s.encode_utf16().collect();
        unsafe {
            self.brush.SetColor(&color);
            let brush: &ID2D1Brush = &self.brush.cast().unwrap();
            self.rt.DrawText(&wide, f, &rect, brush, D2D1_DRAW_TEXT_OPTIONS_CLIP, DWRITE_MEASURING_MODE_NATURAL);
        }
    }

    fn fill_rr(&self, rect: D2D_RECT_F, radius: f32, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            let rr = D2D1_ROUNDED_RECT { rect, radiusX: radius, radiusY: radius };
            self.rt.FillRoundedRectangle(&rr, &self.brush);
        }
    }

    fn fill(&self, rect: D2D_RECT_F, color: D2D1_COLOR_F) {
        unsafe {
            self.brush.SetColor(&color);
            self.rt.FillRectangle(&rect, &self.brush);
        }
    }

    /// Draw the panel: body occupies `[margin, margin + panel_w] x [0, panel_h]`,
    /// with a soft shadow in the margin around it.
    pub fn draw(&mut self, frame: &Frame, scale: f32, margin: i32, panel_w: i32, panel_h: i32, vm: &ViewModel) -> Result<()> {
        self.fonts(scale)?;
        let (w, h) = (frame.width, frame.height);
        unsafe {
            self.rt.BindDC(frame.hdc, &RECT { left: 0, top: 0, right: w, bottom: h })?;
            self.rt.BeginDraw();
            self.rt.Clear(Some(&D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a: 0.0 }));
        }

        let m = margin as f32;
        let (pw, ph) = (panel_w as f32, panel_h as f32);
        let radius = 14.0 * scale;

        // Shadow: stacked translucent rounded rects growing outward.
        let layers = 14;
        for i in (1..=layers).rev() {
            let spread = i as f32 * (m / layers as f32) * 0.85;
            let a = 0.035 * (1.0 - i as f32 / (layers as f32 + 2.0));
            let r = D2D_RECT_F { left: m - spread, top: -spread, right: m + pw + spread, bottom: ph + spread * 0.9 };
            self.fill_rr(r, radius + spread, D2D1_COLOR_F { r: 0.0, g: 0.0, b: 0.0, a });
        }

        // Body with rounded bottom corners only.
        let body = D2D_RECT_F { left: m, top: 0.0, right: m + pw, bottom: ph };
        self.fill_rr(body, radius, BG);
        self.fill(D2D_RECT_F { left: m, top: 0.0, right: m + pw, bottom: radius }, BG);
        // Hairline highlight at the bottom edge for depth.
        self.fill_rr(D2D_RECT_F { left: m + 1.0, top: ph - 2.0 * scale, right: m + pw - 1.0, bottom: ph }, radius, rgba(255, 255, 255, 0.06));

        let fonts = &self.fonts.as_ref().unwrap().1;
        let x0 = m + PAD * scale;
        let x1 = m + pw - PAD * scale;
        let mut y = (PAD - 2.0) * scale;

        for b in &vm.bars {
            let label_h = 18.0 * scale;
            let (label_color, right_color) = match b.state {
                BarState::Fresh => (TEXT, MUTED),
                BarState::Stale => (MUTED, MUTED),
                BarState::Expired | BarState::Missing => (MUTED, rgba(220, 160, 90, 1.0)),
            };
            self.text(&b.label, &fonts.label, D2D_RECT_F { left: x0, top: y, right: x1 - 60.0 * scale, bottom: y + label_h }, label_color);
            self.text(&b.right, &fonts.label_right, D2D_RECT_F { left: x0 + 90.0 * scale, top: y + 1.0 * scale, right: x1, bottom: y + label_h }, right_color);
            let by = y + label_h + 5.0 * scale;
            let bh = 8.0 * scale;
            let pct_w = 46.0 * scale;
            let track = D2D_RECT_F { left: x0, top: by, right: x1 - pct_w, bottom: by + bh };
            self.fill_rr(track, bh / 2.0, TRACK);
            if let Some(p) = b.pct {
                let fill_color = match (b.state, p) {
                    (BarState::Stale, _) => rgba(150, 152, 160, 1.0),
                    (_, p) if p >= 90.0 => rgba(229, 87, 87, 1.0),
                    (_, p) if p >= 70.0 => rgba(232, 168, 66, 1.0),
                    _ => rgba(217, 119, 87, 1.0),
                };
                let fw = ((track.right - track.left) * (p as f32 / 100.0)).max(bh);
                self.fill_rr(D2D_RECT_F { left: track.left, top: by, right: track.left + fw, bottom: by + bh }, bh / 2.0, fill_color);
                let pct_rect = D2D_RECT_F { left: x1 - pct_w, top: by - 8.0 * scale, right: x1, bottom: by + bh + 8.0 * scale };
                self.text(&format!("{:.0}%", p), &fonts.pct, pct_rect, if b.state == BarState::Stale { MUTED } else { TEXT });
            } else {
                let pct_rect = D2D_RECT_F { left: x1 - pct_w, top: by - 8.0 * scale, right: x1, bottom: by + bh + 8.0 * scale };
                self.text("--", &fonts.pct, pct_rect, MUTED);
            }
            y += ROW_BAR * scale;
        }

        y += 8.0 * scale;
        let rt_h = ROW_TEXT * scale;
        self.text(&vm.today, &fonts.body, D2D_RECT_F { left: x0, top: y, right: x1, bottom: y + rt_h }, TEXT);
        y += rt_h;
        self.text(&vm.today_sub, &fonts.small, D2D_RECT_F { left: x0, top: y, right: x1, bottom: y + rt_h }, MUTED);
        y += rt_h;

        if vm.expanded {
            y += 6.0 * scale;
            for (left, right) in &vm.detail {
                let row = D2D_RECT_F { left: x0, top: y, right: x1, bottom: y + rt_h };
                if left.is_empty() {
                    self.text(right, &fonts.header, row, rgba(217, 119, 87, 1.0));
                } else {
                    self.text(left, &fonts.body, D2D_RECT_F { right: x0 + 110.0 * scale, ..row }, TEXT);
                    self.text(right, &fonts.body_right, D2D_RECT_F { left: x0 + 100.0 * scale, ..row }, MUTED);
                }
                y += rt_h;
            }
        }

        y += 6.0 * scale;
        let footer_color = if vm.footer_warn { rgba(232, 168, 66, 1.0) } else { MUTED };
        self.text(&vm.footer, &fonts.small, D2D_RECT_F { left: x0, top: y, right: x1, bottom: y + rt_h }, footer_color);

        unsafe {
            self.rt.EndDraw(None, None)?;
        }
        Ok(())
    }
}
