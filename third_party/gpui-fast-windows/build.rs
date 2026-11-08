// Modified by Longbridge for gpui-fast.
#![allow(clippy::disallowed_methods, reason = "build scripts are exempt")]

// Pets patch: шейдеры компилируются при сборке через D3DCompile (d3dcompiler_47.dll есть в
// любой Windows, fxc.exe не нужен) и вшиваются байткодом: компиляция при запуске стоила
// ~250 мс первого кадра (Урфин, G-07). Сборка не на Windows (кросс-сборка) — по-старому,
// компиляция при запуске (`cfg(shaders_runtime)`). Источник основного модуля — шейдер
// gpui-fast со скруглением клипа (`fast_rounded_clip_shader::stage`, в OUT_DIR).
#[path = "src/fast/rounded_clip_shader_build.rs"]
mod fast_rounded_clip_shader;

fn main() {
    crate::fast_rounded_clip_shader::stage();
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=src/shaders.hlsl");
    println!("cargo:rerun-if-changed=src/color_text_raster.hlsl");
    println!("cargo:rerun-if-changed=src/alpha_correction.hlsl");
    println!("cargo::rustc-check-cfg=cfg(shaders_runtime)");
    #[cfg(windows)]
    shaders::compile();
    #[cfg(not(windows))]
    println!("cargo:rustc-cfg=shaders_runtime");
}

#[cfg(windows)]
mod shaders {
    use std::fmt::Write as _;
    use std::path::PathBuf;

    use windows::Win32::Graphics::Direct3D::Fxc::{D3DCOMPILE_OPTIMIZATION_LEVEL3, D3DCompile};
    use windows::Win32::Graphics::Direct3D::ID3DBlob;
    use windows::core::PCSTR;

    const MODULES: [&str; 9] = [
        "quad",
        "shadow",
        "underline",
        "path_rasterization",
        "path_sprite",
        "monochrome_sprite",
        "subpixel_sprite",
        "polychrome_sprite",
        "emoji_rasterization",
    ];

    pub fn compile() {
        let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
        let alpha = include_str!("src/alpha_correction.hlsl");
        // Pets patch: основной модуль — сгенерированный gpui-fast шейдер (alpha_correction уже вставлен).
        let main = std::fs::read_to_string(crate::fast_rounded_clip_shader::generated_path(&out))
            .expect("rounded clip shader");
        let emoji =
            include_str!("src/color_text_raster.hlsl").replace("#include \"alpha_correction.hlsl\"", alpha);
        let mut rs = String::new();
        for module in MODULES {
            let source = if module == "emoji_rasterization" { &emoji } else { &main };
            for (stage, target) in [("vertex", "vs_4_1"), ("fragment", "ps_4_1")] {
                let bytes = compile_one(source, &format!("{module}_{stage}"), target);
                let file = format!("{module}_{stage}.cso");
                std::fs::write(out.join(&file), &bytes).expect("write shader");
                let _ = writeln!(
                    rs,
                    "const {}_{}_BYTES: &[u8] = include_bytes!(concat!(env!(\"OUT_DIR\"), \"/{file}\"));",
                    module.to_uppercase(),
                    stage.to_uppercase()
                );
            }
        }
        std::fs::write(out.join("shaders_bytes.rs"), rs).expect("write shaders_bytes.rs");
    }

    fn compile_one(source: &str, entry: &str, target: &str) -> Vec<u8> {
        let entry_c = format!("{entry}\0");
        let target_c = format!("{target}\0");
        let mut blob: Option<ID3DBlob> = None;
        let mut errors: Option<ID3DBlob> = None;
        // SAFETY: строки и буферы живы на время вызова; блобы — COM-объекты d3dcompiler.
        unsafe {
            let ret = D3DCompile(
                source.as_ptr().cast(),
                source.len(),
                PCSTR::null(),
                None,
                None,
                PCSTR::from_raw(entry_c.as_ptr()),
                PCSTR::from_raw(target_c.as_ptr()),
                D3DCOMPILE_OPTIMIZATION_LEVEL3,
                0,
                &mut blob,
                Some(&mut errors),
            );
            if ret.is_err() {
                let msg = errors.map_or_else(
                    || format!("{ret:?}"),
                    |e| {
                        std::ffi::CStr::from_ptr(e.GetBufferPointer().cast())
                            .to_string_lossy()
                            .into_owned()
                    },
                );
                panic!("shader {entry}: {msg}");
            }
            let blob = blob.expect("shader blob");
            std::slice::from_raw_parts(blob.GetBufferPointer().cast::<u8>(), blob.GetBufferSize())
                .to_vec()
        }
    }
}
