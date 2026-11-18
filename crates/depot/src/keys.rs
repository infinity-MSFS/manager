//! Storage keys. Clients derive pack keys from the manifest rather than
//! reading them from it, so a manifest can never point outside its
//! product's depot.

use crate::Hash;

pub fn depot_prefix(product_id: &str) -> String {
    format!("depots/{product_id}/")
}

pub fn pack_key(product_id: &str, pack: &Hash) -> String {
    format!("depots/{product_id}/packs/{}.pack", pack.to_hex())
}

pub fn manifest_key(product_id: &str, channel: &str, version: &str, manifest: &Hash) -> String {
    format!(
        "depots/{product_id}/manifests/{channel}/{version}/{}.manifest.zst",
        manifest.to_hex()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_sit_under_the_product() {
        let h = Hash::of(b"x");
        assert!(pack_key("t38", &h).starts_with(&depot_prefix("t38")));
        assert!(manifest_key("t38", "beta", "1.0.0", &h).starts_with(&depot_prefix("t38")));
        assert!(pack_key("t38", &h).ends_with(".pack"));
    }
}
