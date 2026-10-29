//! Embedded provider artwork from the pinned CodexBar resources; see THIRD_PARTY_NOTICES.md.

use std::sync::{Arc, LazyLock};

use gpui::{Image, ImageFormat, IntoElement, Pixels, Styled, img};
use tokenbubble_core::model::ProviderId;

pub(crate) fn provider_icon(provider: ProviderId, size: Pixels) -> impl IntoElement {
    static CODEX: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Png,
            include_bytes!("../assets/codex.png").to_vec(),
        ))
    });
    static CLAUDE: LazyLock<Arc<Image>> = LazyLock::new(|| {
        Arc::new(Image::from_bytes(
            ImageFormat::Svg,
            include_bytes!("../assets/claude.svg").to_vec(),
        ))
    });
    let image = match provider {
        ProviderId::Codex => &*CODEX,
        ProviderId::Claude => &*CLAUDE,
    };
    img(image.clone()).size(size).flex_none()
}
