//! Windows builds: embed assets/icon.ico as the .exe's icon. Writes a compiled resource file
//! (.res) directly, so no resource compiler is needed; both MSVC's linker and lld (used by the
//! zig cross-build) accept a .res file as a link input.

fn main() {
    println!("cargo:rerun-if-changed=assets/icon.ico");
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let ico = std::fs::read("assets/icon.ico").expect("read assets/icon.ico");
    let res = std::path::Path::new(&std::env::var("OUT_DIR").unwrap()).join("icon.res");
    std::fs::write(&res, icon_res(&ico)).expect("write icon.res");
    println!("cargo:rustc-link-arg-bins={}", res.display());
}

const RT_ICON: u16 = 3;
const RT_GROUP_ICON: u16 = 14;

fn u16_at(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

fn u32_at(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]])
}

/// One .res entry with a numeric type and name, padded to 4 bytes.
fn entry(out: &mut Vec<u8>, kind: u16, id: u16, flags: u16, data: &[u8]) {
    out.extend((data.len() as u32).to_le_bytes()); // DataSize
    out.extend(32u32.to_le_bytes()); // HeaderSize
    out.extend([0xFF, 0xFF]);
    out.extend(kind.to_le_bytes());
    out.extend([0xFF, 0xFF]);
    out.extend(id.to_le_bytes());
    out.extend(0u32.to_le_bytes()); // DataVersion
    out.extend(flags.to_le_bytes()); // MemoryFlags
    out.extend(0x0409u16.to_le_bytes()); // LanguageId: en-US
    out.extend(0u32.to_le_bytes()); // Version
    out.extend(0u32.to_le_bytes()); // Characteristics
    out.extend(data);
    while out.len() % 4 != 0 {
        out.push(0);
    }
}

/// .ico file → .res with one RT_ICON per image and an RT_GROUP_ICON (id 1) listing them.
fn icon_res(ico: &[u8]) -> Vec<u8> {
    assert!(u16_at(ico, 2) == 1, "assets/icon.ico isn't an icon file");
    let count = u16_at(ico, 4) as usize;
    // A .res file starts with an empty entry.
    let mut out = vec![0, 0, 0, 0, 32, 0, 0, 0, 0xFF, 0xFF, 0, 0, 0xFF, 0xFF, 0, 0];
    out.extend([0u8; 16]);
    let mut group = Vec::new();
    group.extend(0u16.to_le_bytes());
    group.extend(1u16.to_le_bytes());
    group.extend((count as u16).to_le_bytes());
    for i in 0..count {
        let e = 6 + i * 16;
        let (size, offset) = (u32_at(ico, e + 8) as usize, u32_at(ico, e + 12) as usize);
        let id = i as u16 + 1;
        entry(&mut out, RT_ICON, id, 0x1010, &ico[offset..offset + size]);
        group.extend(&ico[e..e + 12]); // width, height, colours, reserved, planes, bit count, size
        group.extend(id.to_le_bytes());
    }
    entry(&mut out, RT_GROUP_ICON, 1, 0x1030, &group);
    out
}
