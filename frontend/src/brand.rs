use infinity_ui::BlobPalette;
use infinity_ui::prelude::*;

const LEFT_LOOP: &str = "M280.8 728.9c-57.7 0.2-113.1-22.7-153.7-63.6-84.3-84.7-84.3-222.4-0.1-307 40.7-41 96.1-63.9 153.9-63.7 58.1 0 112.7 22.6 153.7 63.6L550 473.7c14 13.5 19.6 33.5 14.7 52.3-4.9 18.8-19.6 33.5-38.4 38.4-18.8 4.9-38.8-0.7-52.3-14.7L358.5 434.3c-41.5-41.5-113.9-41.5-155.3 0-42.5 42.9-42.5 112.1 0.1 155 41.2 41.2 113.7 41.4 155.1-0.1l14-14c21-21 55.1-21 76.1 0 21 21 21 55.1 0 76.1l-14 14c-40.7 40.9-96.1 63.8-153.7 63.6z m0 0";
const RIGHT_LOOP: &str = "M743.1 728.9c-57.7 0.2-113-22.7-153.7-63.6L473.9 549.8c-14-13.5-19.6-33.5-14.7-52.3 4.9-18.8 19.6-33.5 38.4-38.4 18.8-4.9 38.8 0.7 52.3 14.7l115.5 115.5c41.5 41.6 113.9 41.5 155.3 0 42.5-42.9 42.5-112.1-0.1-155-41.2-41.3-113.7-41.4-155.1 0.1l-14 14c-21 21-55.1 21-76.1 0-21-21-21-55.1 0-76.1l14-14c40.7-40.9 96-63.8 153.7-63.6 58.1 0 112.7 22.6 153.7 63.6 84.3 84.7 84.3 222.4 0.1 307-40.7 40.8-96 63.8-153.8 63.6z m0 0";

#[component]
pub(crate) fn InfinityMark(#[prop(into)] class: String) -> impl IntoView {
    view! {
        <svg class=class viewBox="0 0 1024 1024" xmlns="http://www.w3.org/2000/svg" aria-hidden="true">
            <path d=LEFT_LOOP fill="white" />
            <path d=RIGHT_LOOP fill="white" />
        </svg>
    }
}

pub(crate) const PALETTE: BlobPalette = BlobPalette {
    background_start: "rgb(4, 32, 26)",
    background_end: "rgb(0, 0, 0)",
    colors: [
        "52, 211, 153",
        "255, 255, 255",
        "16, 185, 129",
        "45, 212, 191",
        "110, 231, 183",
    ],
    size: "80%",
    blend: "hard-light",
};

pub(crate) fn ramp() -> String {
    format!(
        "background: linear-gradient(40deg, {}, {});",
        PALETTE.background_start, PALETTE.background_end
    )
}
