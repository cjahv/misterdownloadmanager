/// Cache the system glyph mask at the display scale, including misses on older macOS.
pub fn paint_mac_symbol(
    ui: &egui::Ui,
    painter: &egui::Painter,
    need: &str,
    rect: egui::Rect,
    ink: egui::Color32,
) -> bool {
    let name = match need {
        "tb-add-url" => "plus",
        "tb-add-torrent" => "link",
        "tb-resume" => "play",
        "tb-stop" => "stop",
        "tb-stop-all" => "hand.raised",
        "tb-delete" => "trash",
        "tb-delete-completed" => "checkmark.circle",
        "tb-options" => "gearshape",
        "tb-scheduler" => "clock",
        "tb-start-queue" => "play.rectangle.on.rectangle",
        "tb-stop-queue" => "stop.circle",
        "cat-all" => "tray.2",
        "cat-unfinished" => "arrow.down.circle",
        "cat-finished" => "checkmark.circle",
        "cat-queues" => "list.bullet",
        "cat-music" => "music.note",
        "cat-programs" => "app",
        "cat-compressed" => "archivebox",
        "cat-video" => "film",
        "cat-documents" => "doc.text",
        "cat-torrents" => "link",
        _ => return false,
    };
    let side = (rect.width() * ui.ctx().pixels_per_point()).round() as u32;
    let key = egui::Id::unique(("mac-symbol", name, side));
    let cached = ui
        .ctx()
        .data(|d| d.get_temp::<Option<egui::TextureHandle>>(key));
    let texture = cached.unwrap_or_else(|| {
        let texture = mac_symbol(name, side).map(|mask| {
            ui.ctx()
                .load_texture(name, mask, egui::TextureOptions::LINEAR)
        });
        ui.ctx().data_mut(|d| d.insert_temp(key, texture.clone()));
        texture
    });
    if let Some(texture) = texture {
        painter.image(
            texture.id(),
            rect,
            egui::Rect::from_min_max(egui::Pos2::ZERO, egui::pos2(1.0, 1.0)),
            ink,
        );
        true
    } else {
        false
    }
}

fn mac_symbol(name: &str, px: u32) -> Option<egui::ColorImage> {
    use objc2_app_kit::NSImage;
    use objc2_foundation::NSString;
    let image = NSImage::imageWithSystemSymbolName_accessibilityDescription(
        &NSString::from_str(name),
        None,
    )?;
    let crate::fileicons::Native::Rgba { side, rgba, .. } = super::raster_mac_image(&image, px)?
    else {
        return None;
    };
    Some(egui::ColorImage::new(
        [side as usize; 2],
        rgba.as_chunks::<4>()
            .0
            .iter()
            .map(|pixel| egui::Color32::from_white_alpha(pixel[3]))
            .collect(),
    ))
}
