/// Fixture-only Annex B splitting. Real transport already supplies complete AUs.
#[cfg(any(target_os = "windows",target_os="linux"))]
pub fn access_units(bytes: &[u8]) -> Vec<&[u8]> {
    let mut starts = Vec::new();
    let mut i = 0;
    while i + 3 < bytes.len() {
        let prefix = if bytes[i..].starts_with(&[0, 0, 0, 1]) {
            4
        } else if bytes[i..].starts_with(&[0, 0, 1]) {
            3
        } else {
            i += 1;
            continue;
        };
        if i + prefix + 2 <= bytes.len() {
            starts.push((i, i + prefix));
        }
        i += prefix;
    }
    let mut bounds = vec![0];
    let mut had_vcl = false;
    for (offset, header) in starts {
        let nal = (bytes[header] >> 1) & 63;
        let vcl = nal <= 31;
        let first = vcl && header + 2 < bytes.len() && bytes[header + 2] & 0x80 != 0;
        if had_vcl && (first || matches!(nal, 32..=35 | 39)) {
            if offset > *bounds.last().unwrap() {
                bounds.push(offset);
            }
            had_vcl = false;
        }
        if vcl {
            had_vcl = true;
        }
    }
    bounds.push(bytes.len());
    bounds
        .windows(2)
        .filter_map(|b| (b[1] > b[0]).then_some(&bytes[b[0]..b[1]]))
        .collect()
}
