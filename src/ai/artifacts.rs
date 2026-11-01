//! Everything downloaded at run time, pinned by size and SHA-256.

use super::backend::{Backend, order};

/// A single downloadable file.
pub struct Remote {
    pub file_name: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArchiveFormat {
    Zip,
    TarGz,
}

/// One library inside an archive, extracted under a fixed name. Linux names are
/// SONAMEs: dependents find an already loaded library by its SONAME.
pub struct ArchiveFile {
    pub entry: &'static str,
    pub name: &'static str,
    pub sha256: &'static str,
    pub size: u64,
}

/// A downloadable archive, of which only the listed files are used.
pub struct Archive {
    /// Cache directory name; changes whenever the content does.
    pub id: &'static str,
    pub url: &'static str,
    pub sha256: &'static str,
    pub size: u64,
    pub format: ArchiveFormat,
    pub files: &'static [ArchiveFile],
}

/// An execution provider packaged as a plugin library.
pub struct Plugin {
    pub archive: &'static Archive,
    pub library: &'static str,
    /// Loaded by absolute path first, in this order, so the plugin's
    /// dependencies resolve without touching the library search path.
    pub preload: &'static [&'static str],
    /// Name the plugin registers its devices under.
    pub ep_name: &'static str,
}

const WEBGPU_EP: &str = "WebGpuExecutionProvider";
const TENSORRT_RTX_EP: &str = "NvTensorRTRTXExecutionProvider";

pub static DEJPEG_V2: Remote = Remote {
    file_name: "dejpeg-v2.0.0.onnx",
    url: "https://github.com/Miosp/dejpeg/releases/download/v2.0.0/dejpeg-v2.onnx",
    sha256: "cc345b6ce3a4b70856358911fed69ccebe939c1381aa94d969f15e2d81394690",
    size: 15_376_612,
};

static CORE_LINUX_X64: Archive = Archive {
    id: "onnxruntime-1.31.1-linux-x64",
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-linux-x64-1.31.1.tgz",
    sha256: "c849e36a1ef3e22d4ff217b9d1ce4a3cf5b9467b975610913ae959ad5b16bdc9",
    size: 11_413_655,
    format: ArchiveFormat::TarGz,
    files: &[ArchiveFile {
        entry: "onnxruntime-linux-x64-1.31.1/lib/libonnxruntime.so.1.31.1",
        name: "libonnxruntime.so.1.31.1",
        sha256: "a96f132e5eb5ca04e23a7d2fc1078fb95aa591e8d71d0f237702ece3a1e0f4ea",
        size: 29_345_384,
    }],
};

static CORE_LINUX_ARM64: Archive = Archive {
    id: "onnxruntime-1.31.1-linux-aarch64",
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-linux-aarch64-1.31.1.tgz",
    sha256: "5317505e2da99be65f8e2775ba3c2afd6d2961fd78aba1bd8f56159f48820c78",
    size: 10_332_381,
    format: ArchiveFormat::TarGz,
    files: &[ArchiveFile {
        entry: "onnxruntime-linux-aarch64-1.31.1/lib/libonnxruntime.so.1.31.1",
        name: "libonnxruntime.so.1.31.1",
        sha256: "7e17c55d675be73f67a218dbecb55522395f61dafb3da03ebc7ade56a4c2f813",
        size: 25_266_408,
    }],
};

static CORE_MACOS_ARM64: Archive = Archive {
    id: "onnxruntime-1.31.1-osx-arm64",
    url: "https://github.com/microsoft/onnxruntime/releases/download/v1.31.1/onnxruntime-osx-arm64-1.31.1.tgz",
    sha256: "2e1344a1926ee50eb25b38153cb6992c3567515aef0e677a1a520a1798896d6f",
    size: 36_936_435,
    format: ArchiveFormat::TarGz,
    files: &[ArchiveFile {
        entry: "onnxruntime-osx-arm64-1.31.1/lib/libonnxruntime.1.31.1.dylib",
        name: "libonnxruntime.1.31.1.dylib",
        sha256: "17500d6b1e37e70f43a2c57d94233a1bc29fbcb297be0a5f8c7c13184f3307ba",
        size: 44_797_256,
    }],
};

const DML_CORE_URL: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.ml.onnxruntime.directml/1.24.4/microsoft.ml.onnxruntime.directml.1.24.4.nupkg";
const DML_CORE_SHA256: &str = "57e9f11b73437bef7a309496135d4c1f96b1a8e9ddba60013fa27bfc1d788681";
const DML_CORE_SIZE: u64 = 12_458_649;

static CORE_WINDOWS_X64: Archive = Archive {
    id: "onnxruntime-directml-1.24.4-win-x64",
    url: DML_CORE_URL,
    sha256: DML_CORE_SHA256,
    size: DML_CORE_SIZE,
    format: ArchiveFormat::Zip,
    files: &[ArchiveFile {
        entry: "runtimes/win-x64/native/onnxruntime.dll",
        name: "onnxruntime.dll",
        sha256: "e7eedec6a6f26dc39dc948276a75ef6d2bee3fff944d874ceed0bbd3b97bff40",
        size: 17_328_152,
    }],
};

static CORE_WINDOWS_ARM64: Archive = Archive {
    id: "onnxruntime-directml-1.24.4-win-arm64",
    url: DML_CORE_URL,
    sha256: DML_CORE_SHA256,
    size: DML_CORE_SIZE,
    format: ArchiveFormat::Zip,
    files: &[ArchiveFile {
        entry: "runtimes/win-arm64/native/onnxruntime.dll",
        name: "onnxruntime.dll",
        sha256: "939fa0be435d253eadd9e394ee94ad6b6ac1878c83b64118bb4bda669eb0e28c",
        size: 17_546_784,
    }],
};

const DIRECTML_URL: &str = "https://api.nuget.org/v3-flatcontainer/microsoft.ai.directml/1.15.4/microsoft.ai.directml.1.15.4.nupkg";
const DIRECTML_SHA256: &str = "4e7cb7ddce8cf837a7a75dc029209b520ca0101470fcdf275c1f49736a3615b9";
const DIRECTML_SIZE: u64 = 202_292_617;
pub const DIRECTML_DLL: &str = "DirectML.dll";

static DIRECTML_X64: Archive = Archive {
    id: "directml-1.15.4-win-x64",
    url: DIRECTML_URL,
    sha256: DIRECTML_SHA256,
    size: DIRECTML_SIZE,
    format: ArchiveFormat::Zip,
    files: &[ArchiveFile {
        entry: "bin/x64-win/DirectML.dll",
        name: DIRECTML_DLL,
        sha256: "9c9e6d822561c6c41b90e6994b3e8857cf1d66dbfb1e0c4c799c7c89b4e92da1",
        size: 18_527_776,
    }],
};

static DIRECTML_ARM64: Archive = Archive {
    id: "directml-1.15.4-win-arm64",
    url: DIRECTML_URL,
    sha256: DIRECTML_SHA256,
    size: DIRECTML_SIZE,
    format: ArchiveFormat::Zip,
    files: &[ArchiveFile {
        entry: "bin/arm64-win/DirectML.dll",
        name: DIRECTML_DLL,
        sha256: "77b0db83ff903f2323f5caf538499d75af6038bbea23b7959f7d232d9a4ab9d4",
        size: 18_444_832,
    }],
};

static WEBGPU_LINUX_X64: Plugin = Plugin {
    archive: &Archive {
        id: "webgpu-ep-0.5.0-linux-x64",
        url: "https://files.pythonhosted.org/packages/e2/1d/ac0ccbe8bd6435b6e1946fea964790ffa3d73ee2513775e90958fd6b860e/onnxruntime_ep_webgpu-0.5.0-py3-none-manylinux_2_28_x86_64.whl",
        sha256: "b6baa3e223d9a679023388b1179e5597b6bc0d282ba72f2afe38d46ef37deeb4",
        size: 6_935_993,
        format: ArchiveFormat::Zip,
        files: &[ArchiveFile {
            entry: "onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.so",
            name: "libonnxruntime_providers_webgpu.so",
            sha256: "fc98b10c7d03537694ff4ea6ebca79ed3b41ca71e316dac3292257ee5f85c95d",
            size: 16_613_128,
        }],
    },
    library: "libonnxruntime_providers_webgpu.so",
    preload: &[],
    ep_name: WEBGPU_EP,
};

static WEBGPU_LINUX_ARM64: Plugin = Plugin {
    archive: &Archive {
        id: "webgpu-ep-0.5.0-linux-aarch64",
        url: "https://files.pythonhosted.org/packages/93/ca/71f305dc822d62ef33d7f3862ba7058a0099086112d9ab679d8252e7b77d/onnxruntime_ep_webgpu-0.5.0-py3-none-manylinux_2_28_aarch64.whl",
        sha256: "d39e0769f8b2ff7a4b04a492ef882510474d52cf722408d393569b36558b2829",
        size: 6_107_612,
        format: ArchiveFormat::Zip,
        files: &[ArchiveFile {
            entry: "onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.so",
            name: "libonnxruntime_providers_webgpu.so",
            sha256: "603830725756371b3f6eb31e5ef46a2b0e2f333f09ce2fc24e37661ce2fe69fb",
            size: 14_057_376,
        }],
    },
    library: "libonnxruntime_providers_webgpu.so",
    preload: &[],
    ep_name: WEBGPU_EP,
};

static WEBGPU_MACOS: Plugin = Plugin {
    archive: &Archive {
        id: "webgpu-ep-0.5.0-macos",
        url: "https://files.pythonhosted.org/packages/2c/c3/e047d38981e7d6f198f92b488e2c9cd12e45c73017ec851996ea69778922/onnxruntime_ep_webgpu-0.5.0-py3-none-macosx_14_0_universal2.whl",
        sha256: "c9a757c7afaf114fde86993420810ff616e2d3046d42ae419bb0c8b9ebcf57fb",
        size: 4_864_395,
        format: ArchiveFormat::Zip,
        files: &[ArchiveFile {
            entry: "onnxruntime_ep_webgpu/libonnxruntime_providers_webgpu.dylib",
            name: "libonnxruntime_providers_webgpu.dylib",
            sha256: "796e02f9bc1d96c3a163f27a139b7a9358dd40ba850fd3788134960db28994ab",
            size: 14_411_424,
        }],
    },
    library: "libonnxruntime_providers_webgpu.dylib",
    preload: &[],
    ep_name: WEBGPU_EP,
};

static WEBGPU_WINDOWS_X64: Plugin = Plugin {
    archive: &Archive {
        id: "webgpu-ep-0.5.0-win-x64",
        url: "https://files.pythonhosted.org/packages/7e/36/9935a5b7851099da3da81d9e04f3021bcc61a5a1039e6c098cf08e5c6607/onnxruntime_ep_webgpu-0.5.0-py3-none-win_amd64.whl",
        sha256: "713b94e7b4ce5979a356e19b926ecba2c0d4ba24f857ef4bbb11c75c37cc5b61",
        size: 13_608_886,
        format: ArchiveFormat::Zip,
        files: &[
            ArchiveFile {
                entry: "onnxruntime_ep_webgpu/dxcompiler.dll",
                name: "dxcompiler.dll",
                sha256: "40fff09e9f79f8b3c8f634b010ef1c22c79aef6bf0e02e702e7efba2532659b1",
                size: 17_986_400,
            },
            ArchiveFile {
                entry: "onnxruntime_ep_webgpu/onnxruntime_providers_webgpu.dll",
                name: "onnxruntime_providers_webgpu.dll",
                sha256: "dbbbbe7e22e8323167ca2f4c3f5bd7c72a2cc2ff66d00afe0c80251dadcf6ff0",
                size: 13_426_488,
            },
        ],
    },
    library: "onnxruntime_providers_webgpu.dll",
    preload: &["dxcompiler.dll"],
    ep_name: WEBGPU_EP,
};

static WEBGPU_WINDOWS_ARM64: Plugin = Plugin {
    archive: &Archive {
        id: "webgpu-ep-0.5.0-win-arm64",
        url: "https://files.pythonhosted.org/packages/b1/f2/f62eb9fde3b790507da4e1c4a3f366ea8e69b3b424fe70e97c898dd8f9c2/onnxruntime_ep_webgpu-0.5.0-py3-none-win_arm64.whl",
        sha256: "928b3849a52403b5ccee3264a721a104c0fa3ebebb1e725d486e9dc6611f848e",
        size: 14_582_521,
        format: ArchiveFormat::Zip,
        files: &[
            ArchiveFile {
                entry: "onnxruntime_ep_webgpu/dxcompiler.dll",
                name: "dxcompiler.dll",
                sha256: "18fde665ae54895c6e815ad7a2af67a2b83c7613c99868e2cbc53561e04a1f1c",
                size: 22_285_664,
            },
            ArchiveFile {
                entry: "onnxruntime_ep_webgpu/onnxruntime_providers_webgpu.dll",
                name: "onnxruntime_providers_webgpu.dll",
                sha256: "dfedf794ec20d46dc9d9b8deaebdb20d474fd2753445d6bec510a98294422653",
                size: 13_846_840,
            },
        ],
    },
    library: "onnxruntime_providers_webgpu.dll",
    preload: &["dxcompiler.dll"],
    ep_name: WEBGPU_EP,
};

const TRT_LINUX_PRELOAD: &[&str] = &[
    "libcudart.so.13",
    "libtensorrt_rtx.so.1",
    "libtensorrt_onnxparser_rtx.so.1",
];
const TRT_WINDOWS_PRELOAD: &[&str] = &["tensorrt_rtx_1_7.dll", "tensorrt_onnxparser_rtx_1_7.dll"];

static TENSORRT_LINUX_X64: Plugin = Plugin {
    archive: &Archive {
        id: "tensorrt-rtx-ep-0.5.0-cu13-linux-x64",
        url: "https://github.com/NVIDIA/TensorRT-RTX-EP-ABI/releases/download/v0.5.0/TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-x86_64.zip",
        sha256: "141b500bbbb6cd8b61d76f25f778e86611de02d3b02a9a2d38620191c85736a7",
        size: 146_510_199,
        format: ArchiveFormat::Zip,
        files: &[
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-x86_64/libcudart.so.13.4.36",
                name: "libcudart.so.13",
                sha256: "82d50fc923566a86ff5449cb190d58007a26280669b4f62be2c284cacb51ed24",
                size: 798_496,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-x86_64/libtensorrt_rtx.so.1.7.1",
                name: "libtensorrt_rtx.so.1",
                sha256: "6980f8946580af43f06ccbd82a06c65d61638ff76329d726764a0a408747be84",
                size: 234_013_248,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-x86_64/libtensorrt_onnxparser_rtx.so.1.7.1",
                name: "libtensorrt_onnxparser_rtx.so.1",
                sha256: "8bc09093b0e460f494d0410d8a2d2739b3e4de441d016e1f8ffa74bfb99a49aa",
                size: 4_097_984,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-x86_64/libtensorrt_plugins.so",
                name: "libtensorrt_plugins.so",
                sha256: "45eb50eb35378a737ba96e63f7aa8b4ef1f7c7e1739f9066fe869a7074d7a0d4",
                size: 69_706_480,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-x86_64/libtensorrt_shim.so",
                name: "libtensorrt_shim.so",
                sha256: "54c78af86decf4341782543140ba6690fada2abafcfda7c324bc4b9c73ebf484",
                size: 1_530_568,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-x86_64/libonnxruntime_providers_nv_tensorrt_rtx.so",
                name: "libonnxruntime_providers_nv_tensorrt_rtx.so",
                sha256: "d83dde283dd583376fa17b49fbc9d7c85674ae1a077f7e5671f79c4e2f056d78",
                size: 5_113_208,
            },
        ],
    },
    library: "libonnxruntime_providers_nv_tensorrt_rtx.so",
    preload: TRT_LINUX_PRELOAD,
    ep_name: TENSORRT_RTX_EP,
};

static TENSORRT_LINUX_ARM64: Plugin = Plugin {
    archive: &Archive {
        id: "tensorrt-rtx-ep-0.5.0-cu13-linux-aarch64",
        url: "https://github.com/NVIDIA/TensorRT-RTX-EP-ABI/releases/download/v0.5.0/TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-aarch64.zip",
        sha256: "2a4aa054d5ddb580d2ffabe1b07977534abb869960d0f88f49df27b5354e8bc0",
        size: 134_888_826,
        format: ArchiveFormat::Zip,
        files: &[
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-aarch64/libcudart.so.13.4.46",
                name: "libcudart.so.13",
                sha256: "20041c8eafd0ec3cdf884d20152d565acbf63b68f417a70be18d78bd2e8f7b86",
                size: 793_872,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-aarch64/libtensorrt_rtx.so.1.7.1",
                name: "libtensorrt_rtx.so.1",
                sha256: "fa95f5785feb6210f872419a5ded4c340b5c22cf23fdace92b01a5627a8da77b",
                size: 213_678_008,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-aarch64/libtensorrt_onnxparser_rtx.so.1.7.1",
                name: "libtensorrt_onnxparser_rtx.so.1",
                sha256: "e0d66b6fe8d7b443d9e10436c2b89ce2435db7e8d34f931971a8a463c50b5084",
                size: 3_361_776,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-aarch64/libtensorrt_plugins.so",
                name: "libtensorrt_plugins.so",
                sha256: "fad6c97904784974ca37a1d5d0bff370943b6ae2ca51f95ed3fc829d9ad49b88",
                size: 65_818_264,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-aarch64/libtensorrt_shim.so",
                name: "libtensorrt_shim.so",
                sha256: "71a1c88ac5a06ad31afad7c2be8378232e5bb8bfe139704eb5a0abe82eb683e1",
                size: 1_347_240,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-linux-aarch64/libonnxruntime_providers_nv_tensorrt_rtx.so",
                name: "libonnxruntime_providers_nv_tensorrt_rtx.so",
                sha256: "e5bfbd98a3845489672481f6632def361d730af6b0b5f45b716fb6a279b7b4f7",
                size: 4_618_848,
            },
        ],
    },
    library: "libonnxruntime_providers_nv_tensorrt_rtx.so",
    preload: TRT_LINUX_PRELOAD,
    ep_name: TENSORRT_RTX_EP,
};

static TENSORRT_WINDOWS_X64: Plugin = Plugin {
    archive: &Archive {
        id: "tensorrt-rtx-ep-0.5.0-cu13-win-x64",
        url: "https://github.com/NVIDIA/TensorRT-RTX-EP-ABI/releases/download/v0.5.0/TensorRT-RTX-EP-ABI-v0.5.0-cu13.zip",
        sha256: "41469e0b21cabf997cbee3638d9aa4400ae19601667e18ccf6628a960fb0aca3",
        size: 112_819_790,
        format: ArchiveFormat::Zip,
        files: &[
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13/tensorrt_rtx_1_7.dll",
                name: "tensorrt_rtx_1_7.dll",
                sha256: "7b8a06d4b66dfe7da7f1ff48a994ad9dcf107cd28b4822d6f25453528c0db6dd",
                size: 229_312_112,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13/tensorrt_onnxparser_rtx_1_7.dll",
                name: "tensorrt_onnxparser_rtx_1_7.dll",
                sha256: "b38d00d107dae534f1fc855df5bc1a04fc0ae25135b5b10d17f9b30cc4c9c411",
                size: 2_329_200,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13/tensorrt_plugins.dll",
                name: "tensorrt_plugins.dll",
                sha256: "50a345da5f5f9ecc214410f65964513f37e9c30e8bfe55eeb9f744bc0db95692",
                size: 51_289_200,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13/onnxruntime_providers_nv_tensorrt_rtx.dll",
                name: "onnxruntime_providers_nv_tensorrt_rtx.dll",
                sha256: "efbf29728e5dbdc148369640cf2eac7215c05deff256484bd0a61d8a96d140a7",
                size: 2_392_176,
            },
        ],
    },
    library: "onnxruntime_providers_nv_tensorrt_rtx.dll",
    preload: TRT_WINDOWS_PRELOAD,
    ep_name: TENSORRT_RTX_EP,
};

static TENSORRT_WINDOWS_ARM64: Plugin = Plugin {
    archive: &Archive {
        id: "tensorrt-rtx-ep-0.5.0-cu13-win-arm64",
        url: "https://github.com/NVIDIA/TensorRT-RTX-EP-ABI/releases/download/v0.5.0/TensorRT-RTX-EP-ABI-v0.5.0-cu13-win-arm64.zip",
        sha256: "7d7fbea88138f77eb3b6ab40fc84326be119b7ea6483bcb85917968819f263dc",
        size: 100_423_694,
        format: ArchiveFormat::Zip,
        files: &[
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-win-arm64/tensorrt_rtx_1_7.dll",
                name: "tensorrt_rtx_1_7.dll",
                sha256: "e2b29af3eb9e5304472d3e9995e56d56bf4e0bea16d4f15ce2b613235d5b943a",
                size: 221_829_744,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-win-arm64/tensorrt_onnxparser_rtx_1_7.dll",
                name: "tensorrt_onnxparser_rtx_1_7.dll",
                sha256: "6d7d798b78385961bd3be95214c6fb7ef2de68f7c73feb3e64c5dce8b130cdac",
                size: 2_410_608,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-win-arm64/tensorrt_plugins.dll",
                name: "tensorrt_plugins.dll",
                sha256: "a6ee1fb5cbbb6ad3c8474ce26170ca733e2c15fa77d9c36ed7d01e17318797ec",
                size: 48_758_384,
            },
            ArchiveFile {
                entry: "TensorRT-RTX-EP-ABI-v0.5.0-cu13-win-arm64/onnxruntime_providers_nv_tensorrt_rtx.dll",
                name: "onnxruntime_providers_nv_tensorrt_rtx.dll",
                sha256: "e0df12155d645a907524df10ea27b043e6aabfafed594343ca02b0a8ccd1c699",
                size: 2_418_288,
            },
        ],
    },
    library: "onnxruntime_providers_nv_tensorrt_rtx.dll",
    preload: TRT_WINDOWS_PRELOAD,
    ep_name: TENSORRT_RTX_EP,
};

/// ONNX Runtime core for a platform.
pub fn core(os: &str, arch: &str) -> Option<&'static Archive> {
    match (os, arch) {
        ("linux", "x86_64") => Some(&CORE_LINUX_X64),
        ("linux", "aarch64") => Some(&CORE_LINUX_ARM64),
        ("macos", "aarch64") => Some(&CORE_MACOS_ARM64),
        ("windows", "x86_64") => Some(&CORE_WINDOWS_X64),
        ("windows", "aarch64") => Some(&CORE_WINDOWS_ARM64),
        _ => None,
    }
}

/// Plugin for a backend; DirectML and CPU live in the core and have none.
pub fn plugin(backend: Backend, os: &str, arch: &str) -> Option<&'static Plugin> {
    match (backend, os, arch) {
        (Backend::Webgpu, "linux", "x86_64") => Some(&WEBGPU_LINUX_X64),
        (Backend::Webgpu, "linux", "aarch64") => Some(&WEBGPU_LINUX_ARM64),
        (Backend::Webgpu, "macos", "aarch64") => Some(&WEBGPU_MACOS),
        (Backend::Webgpu, "windows", "x86_64") => Some(&WEBGPU_WINDOWS_X64),
        (Backend::Webgpu, "windows", "aarch64") => Some(&WEBGPU_WINDOWS_ARM64),
        (Backend::TensorrtRtx, "linux", "x86_64") => Some(&TENSORRT_LINUX_X64),
        (Backend::TensorrtRtx, "linux", "aarch64") => Some(&TENSORRT_LINUX_ARM64),
        (Backend::TensorrtRtx, "windows", "x86_64") => Some(&TENSORRT_WINDOWS_X64),
        (Backend::TensorrtRtx, "windows", "aarch64") => Some(&TENSORRT_WINDOWS_ARM64),
        _ => None,
    }
}

/// `DirectML.dll` package for a Windows architecture.
pub fn directml(arch: &str) -> Option<&'static Archive> {
    match arch {
        "x86_64" => Some(&DIRECTML_X64),
        "aarch64" => Some(&DIRECTML_ARM64),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLATFORMS: [(&str, &str); 5] = [
        ("linux", "x86_64"),
        ("linux", "aarch64"),
        ("macos", "aarch64"),
        ("windows", "x86_64"),
        ("windows", "aarch64"),
    ];
    const SHA256_HEX_LEN: usize = 64;

    fn assert_pin(sha256: &str, size: u64) {
        assert_eq!(sha256.len(), SHA256_HEX_LEN);
        assert!(sha256.bytes().all(|b| b.is_ascii_hexdigit()));
        assert!(size > 0);
    }

    fn assert_archive(archive: &Archive) {
        assert_pin(archive.sha256, archive.size);
        assert!(!archive.files.is_empty());
        for file in archive.files {
            assert_pin(file.sha256, file.size);
            assert!(!file.name.contains(['/', '\\']), "{}", file.name);
        }
    }

    #[test]
    fn every_backend_of_every_platform_has_its_artifacts() {
        for (os, arch) in PLATFORMS {
            assert_archive(core(os, arch).unwrap());
            for &backend in order(os, arch) {
                match backend {
                    Backend::Cpu => {}
                    Backend::Directml => assert_archive(directml(arch).unwrap()),
                    Backend::Webgpu | Backend::TensorrtRtx => {
                        let plugin = plugin(backend, os, arch).unwrap();
                        assert_archive(plugin.archive);
                        let has = |name: &str| plugin.archive.files.iter().any(|f| f.name == name);
                        assert!(has(plugin.library));
                        assert!(plugin.preload.iter().all(|name| has(name)));
                    }
                }
            }
        }
        assert_pin(DEJPEG_V2.sha256, DEJPEG_V2.size);
    }

    #[test]
    fn unsupported_platform_has_no_core() {
        assert!(core("macos", "x86_64").is_none());
    }
}
