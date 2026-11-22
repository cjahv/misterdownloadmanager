//! The Native theme's progress summary. Transfer state and actions belong to the shared dialog.

use super::*;

pub(super) fn meter(p: &egui::Painter, o: Pos2, y: f32, fraction: Option<f64>, status: Status) {
    let width = 512.0;
    let (offset, fill) = match fraction {
        Some(f) => (0.0, width * f.clamp(0.0, 1.0) as f32),
        None if status == Status::Downloading => {
            // An unknown length has activity but no completed fraction. Rely on the
            // download window's normal updates, including while connecting.
            let phase = (p.ctx().input(|i| i.time) % 2.0 - 1.0).abs() as f32;
            (phase * (width - 72.0), 72.0)
        }
        None => return,
    };
    p.rect_filled(r(o, 18.0, y, width, 5.0), 2.5, winui::FIELD_EDGE());
    if fill > 0.0 {
        p.rect_filled(
            r(o, 18.0 + offset, y, fill, 5.0),
            2.5,
            crate::theme::colors().accent,
        );
    }
}

pub(super) fn segments(p: &egui::Painter, o: Pos2, total: Option<u64>, segments: &[(u64, f64)]) {
    let Some(total) = total.filter(|n| *n > 0) else {
        return;
    };
    let width = 512.0;
    p.rect_filled(r(o, 18.0, 253.0, width, 6.0), 3.0, winui::FIELD_EDGE());
    for &(start, done) in segments {
        let offset = |bytes: f64| (bytes / total as f64).clamp(0.0, 1.0) as f32 * width;
        let left = offset(start as f64);
        let right = offset(start as f64 + done);
        if right > left {
            p.rect_filled(
                r(o, 18.0 + left, 253.0, right - left, 6.0),
                1.5,
                crate::theme::colors().accent,
            );
        }
        if left > 0.0 && left < width {
            p.rect_filled(r(o, 18.0 + left, 253.0, 1.0, 6.0), 0.0, winui::BG());
        }
    }
}

pub(super) fn disclosure(ui: &mut Ui, rect: egui::Rect, key: &str, open: bool) -> egui::Response {
    let label = crate::i18n::t(key).trim_matches(['<', '>', ' ']);
    let arrow = if open {
        "▾"
    } else if winui::rtl() {
        "◂"
    } else {
        "▸"
    };
    ui.put(
        rect,
        egui::Button::new(egui::RichText::new(format!("{arrow}  {label}")).color(winui::TEXT()))
            .frame(false),
    )
}

impl Progress {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn mac_status_page(
        &mut self,
        ui: &mut Ui,
        o: Pos2,
        item: &Item,
        snap: Option<&Snapshot>,
        status: &str,
        explained: bool,
        fraction: Option<f64>,
    ) {
        let p = ui.painter().clone();
        let left = o.x + 18.0;
        let right = o.x + 530.0;
        let font = winui::tahoma_title();
        let name = winui::ellipsize_middle(&p, &font, &item.file_name, right - left);
        let rc = winui::text_font(&p, font, left, o.y + 56.0, &name, winui::TEXT());
        winui::copyable(
            ui,
            rc,
            "progress-name",
            &item.file_name,
            name != item.file_name,
        );
        let url = item.file_address();
        let (rc, cut) = winui::text_fit(
            &p,
            left,
            o.y + 75.0,
            &crate::i18n::ltr(url.as_str()),
            winui::MUTED(),
            right,
        );
        winui::copyable(ui, rc, "url", url.as_str(), cut);

        let mut status_right = right;
        if let Some(f) = fraction {
            let percent = crate::i18n::percent(f, 1);
            let (rc, _) = winui::text_right(&p, right, o.y + 102.0, &percent, winui::TEXT());
            status_right -= rc.width() + 12.0;
        }
        let (rc, cut) = winui::text_fit(&p, left, o.y + 102.0, status, winui::TEXT(), status_right);
        winui::copyable(ui, rc, "Status", status, cut);
        if explained {
            let reason = crate::i18n::sentence_start(item.error.as_deref().unwrap_or_default());
            let area = r(o, 18.0, 123.0, 512.0, 48.0);
            winui::paragraph(
                &p.with_clip_rect(area),
                left,
                o.y + 136.0,
                &reason,
                winui::TEXT(),
                right,
                16.0,
            );
            winui::copyable(ui, area, "progress-error", &reason, true);
        } else {
            let mut amount = size3(item.downloaded);
            if let Some(total) = item.size {
                amount.push_str(&format!(" / {}", size3(total)));
            }
            for (label, value, start, end) in [
                ("Downloaded", amount, 18.0, 225.0),
                ("Transfer rate", self.transfer_rate(item), 238.0, 403.0),
                ("Time left", item.time_left(), 416.0, 530.0),
            ] {
                let start = o.x + start;
                let end = o.x + end;
                let label = crate::i18n::t(label);
                let (rc, cut) = winui::text_fit(&p, start, o.y + 137.0, label, winui::MUTED(), end);
                if cut {
                    winui::full_on_hover(ui, rc, ("progress-metric", label), label);
                }
                let value = if value.is_empty() { "—" } else { &value };
                let (rc, cut) = winui::text_fit(&p, start, o.y + 156.0, value, winui::TEXT(), end);
                winui::copyable(ui, rc, label, value, cut);
            }
        }

        let label = crate::i18n::t("Resume capability");
        let value = match item.resumable {
            Some(true) => crate::i18n::t("Yes"),
            Some(false) => crate::i18n::t("No"),
            None => "—",
        };
        let resume = format!("{label}  {value}");
        let (rc, cut) =
            winui::text_fit(&p, left, o.y + 186.0, &resume, winui::MUTED(), o.x + 225.0);
        if cut {
            winui::full_on_hover(ui, rc, "resume-capability", &resume);
        }
        self.server_row(ui, o, item, snap, 238.0);
        self.rename_row(ui, &p, o, 84.0, item);
    }
}
