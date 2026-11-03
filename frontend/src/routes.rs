pub(crate) const LIBRARY: &str = "/";
pub(crate) const SETTINGS: &str = "/settings";
pub(crate) const LOGIN: &str = "/login";
pub(crate) const WELCOME: &str = "/welcome";

pub(crate) fn configure(product_id: &str) -> String {
    format!("/configure/{product_id}")
}

pub(crate) fn liveries(product_id: &str) -> String {
    format!("/liveries/{product_id}")
}
