use infinity_ui::prelude::*;

use super::Page;
use crate::editorial::PageHead;
use crate::routes;

#[component]
pub(crate) fn NotFound() -> impl IntoView {
    view! {
        <Page>
            <PageHead eyebrow="Infinity / 404" title="Nothing at" accent="this address." />
            <Button variant=Variant::Glass href=routes::LIBRARY icon=icons::ARROW_LEFT>
                "Back to library"
            </Button>
        </Page>
    }
}
