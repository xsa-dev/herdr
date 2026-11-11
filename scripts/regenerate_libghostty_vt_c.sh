#!/usr/bin/env bash
set -euo pipefail

# Regenerate pre-transpiled C99 sources for libghostty-vt into vendor/libghostty-vt-c/
# inside a disposable Linux container with pinned Zig 0.16.0 (plus WABT wasm2c installed
# for documented Tier 2 fallback). Zero .a/.so/.wasm/.o binary artifacts are emitted.

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OUT_DIR="${REPO_ROOT}/vendor/libghostty-vt-c"
IMAGE_TAG="${HERDR_VT_GEN_IMAGE:-localhost/herdr-vt-gen:0.16.0}"

detect_container_runtime() {
    if [[ -n "${CONTAINER_RUNTIME:-}" ]]; then
        printf '%s\n' "${CONTAINER_RUNTIME}"
        return
    fi
    if command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
        printf 'docker\n'
        return
    fi
    if command -v podman >/dev/null 2>&1; then
        printf 'podman\n'
        return
    fi
    if [[ -x /opt/podman/bin/podman ]]; then
        printf '/opt/podman/bin/podman\n'
        return
    fi
    echo "error: neither a running docker daemon nor podman was found" >&2
    exit 1
}

RUNTIME="$(detect_container_runtime)"

if [[ "${FORCE_IMAGE_REBUILD:-0}" == "1" ]] || ! "${RUNTIME}" image inspect "${IMAGE_TAG}" >/dev/null 2>&1; then
    echo "==> Building generator container image ${IMAGE_TAG} via ${RUNTIME}..."
    "${RUNTIME}" build -t "${IMAGE_TAG}" - <<'DOCKERFILE'
FROM debian:trixie-slim

RUN apt-get update && apt-get install -y --no-install-recommends \
    binutils \
    ca-certificates \
    clang \
    curl \
    gcc \
    libc6-dev \
    python3 \
    wabt \
    xz-utils \
    && rm -rf /var/lib/apt/lists/*

ARG ZIG_VERSION=0.16.0
RUN set -eux; \
    arch="$(uname -m)"; \
    case "${arch}" in \
        x86_64) \
            zig_arch="x86_64"; \
            zig_sha256="70e49664a74374b48b51e6f3fdfbf437f6395d42509050588bd49abe52ba3d00"; \
            ;; \
        aarch64|arm64) \
            zig_arch="aarch64"; \
            zig_sha256="ea4b09bfb22ec6f6c6ceac57ab63efb6b46e17ab08d21f69f3a48b38e1534f17"; \
            ;; \
        *) echo "unsupported container arch: ${arch}" >&2; exit 1 ;; \
    esac; \
    url="https://ziglang.org/download/${ZIG_VERSION}/zig-${zig_arch}-linux-${ZIG_VERSION}.tar.xz"; \
    curl -fsSL "${url}" -o /tmp/zig.tar.xz; \
    echo "${zig_sha256}  /tmp/zig.tar.xz" | sha256sum -c -; \
    mkdir -p /opt/zig; \
    tar -xJf /tmp/zig.tar.xz -C /opt/zig --strip-components=1; \
    rm -f /tmp/zig.tar.xz; \
    /opt/zig/zig version
DOCKERFILE
fi

STAGE_DIR="$(mktemp -d "${TMPDIR:-/tmp}/herdr-vt-c.XXXXXX")"
trap 'rm -rf "${STAGE_DIR}"' EXIT

ZIG_PKG_MOUNT=()
if [[ -d "${HOME}/.cache/zig/p" ]]; then
    ZIG_PKG_MOUNT=(-v "${HOME}/.cache/zig/p:/host-zig-p:ro")
fi

echo "==> Generating C99 translation units in disposable container..."
"${RUNTIME}" run --rm \
    -v "${REPO_ROOT}:/repo:ro" \
    -v "${STAGE_DIR}:/out" \
    "${ZIG_PKG_MOUNT[@]}" \
    "${IMAGE_TAG}" \
    bash -euo pipefail -c '
export ZIG_GLOBAL_CACHE_DIR=/tmp/zig-global-cache
mkdir -p "${ZIG_GLOBAL_CACHE_DIR}/p"

if [[ -d /host-zig-p ]]; then
    cp -R /host-zig-p/. "${ZIG_GLOBAL_CACHE_DIR}/p/" 2>/dev/null || true
    for archive in "${ZIG_GLOBAL_CACHE_DIR}/p/"*.tar.gz; do
        [[ -e "${archive}" ]] || continue
        pkg_dir="${archive%.tar.gz}"
        if [[ ! -d "${pkg_dir}" ]]; then
            mkdir -p "${pkg_dir}"
            tar -xzf "${archive}" -C "${pkg_dir}" --strip-components=1
        fi
    done
fi

cp -R /repo/vendor/libghostty-vt /tmp/vt
VERSION_STR="$(tr -d "[:space:]" < /tmp/vt/VERSION)"

python3 - << "PY"
from pathlib import Path

build_zig = Path("/tmp/vt/build.zig")
txt = build_zig.read_text()
marker = "\n}\n\nfn addGhosttyH("
if marker not in txt:
    raise SystemExit("expected addGhosttyH anchor in vendor/libghostty-vt/build.zig")

snippet = """
    {
        const emit_c_step = b.step("emit-c", "Emit C99 source for libghostty-vt");
        mod.vt_c.link_libc = true;
        mod.vt_c.strip = true;
        mod.vt.strip = true;
        deps.uucode_mod.optimize = config.optimize;
        deps.uucode_mod.strip = true;
        const c_obj = b.addObject(.{
            .name = "ghostty_vt",
            .root_module = mod.vt_c,
        });
        emit_c_step.dependOn(&b.addInstallFileWithDir(c_obj.getEmittedBin(), .prefix, "ghostty_vt.c").step);
    }
"""
idx = txt.index(marker)
build_zig.write_text(txt[:idx] + snippet + txt[idx:])
PY

cd /tmp/vt
COMMON_FLAGS=(
    emit-c
    -Demit-lib-vt
    -Doptimize=ReleaseFast
    -Dsimd=false
    -Dvt-features=-kitty_graphics
    -Dofmt=c
    "-Dversion-string=${VERSION_STR}"
    -Dstrip=true
)

/opt/zig/zig build "${COMMON_FLAGS[@]}" -Dtarget=x86_64-linux-gnu -p /tmp/out-x86_64
/opt/zig/zig build "${COMMON_FLAGS[@]}" -Dtarget=aarch64-linux-gnu -p /tmp/out-aarch64

cp /opt/zig/lib/zig.h /out/zig.h
cp /tmp/out-x86_64/ghostty_vt.c /out/ghostty_vt_x86_64_linux.c
cp /tmp/out-aarch64/ghostty_vt.c /out/ghostty_vt_aarch64_linux.c

# Fix Zig 0.16.0 C backend (-ofmt=c) codegen bug on @bitCast(@Vector(N, bool)):
# Zig emits `struct vec_N_bool { bool array[N]; }` (N bytes, 1 byte per bool)
# in C, but lowers `@bitCast` to an N-bit integer as `memcpy(&dst, &vec, (N+7)/8)`,
# which copies only the first 1-2 elements (`array[0..K]`) instead of packing all
# N boolean lanes into bits `0..N-1`. We rewrite every such bitcast into an ISO C99
# bit-pack/unpack and reject any unhandled sub-byte vector address-taking.
python3 - << "PY"
import re
from pathlib import Path

BYTE_ALIGNED_ELEMS = {
    "u8", "i8", "u16", "i16", "u32", "i32",
    "u64", "i64", "u128", "i128", "usize", "isize",
    "f16", "f32", "f64",
}

for c_path in [Path("/out/ghostty_vt_x86_64_linux.c"), Path("/out/ghostty_vt_aarch64_linux.c")]:
    lines = c_path.read_text().splitlines()
    subbyte_defs = {}
    for ln in lines:
        m = re.match(r"^struct (vec_(\d+)_([A-Za-z0-9]+)_\d+)\s*\{", ln)
        if m:
            vtype, lanes, elem = m.group(1), int(m.group(2)), m.group(3)
            if elem not in BYTE_ALIGNED_ELEMS:
                subbyte_defs[vtype] = (lanes, elem)

    rewrites = 0
    func_start = None
    for idx, line in enumerate(lines):
        if line and not line[0].isspace() and line.endswith("{") and not line.startswith(("struct ", "union ")):
            func_start = idx
        elif line == "}" and func_start is not None:
            vmap = {}
            smap = {}
            for lno in range(func_start, idx + 1):
                ln = lines[lno]
                mv = re.match(r"^\s+struct (vec_\w+)\s+(\w+);$", ln)
                if mv and mv.group(1) in subbyte_defs:
                    vmap[mv.group(2)] = mv.group(1)
                ms = re.match(r"^\s+((?:uint|int)\d+_t|uintptr_t|intptr_t|bool)\s+(\w+);$", ln)
                if ms:
                    smap[ms.group(2)] = ms.group(1)
            if vmap:
                for lno in range(func_start, idx + 1):
                    ln = lines[lno]
                    for vname, vtype in vmap.items():
                        if f"&{vname}" not in ln:
                            continue
                        mm = re.match(r"^(\s*)memcpy\(&(\w+),\s*&(\w+),\s*(\d+)\);$", ln)
                        if not mm:
                            raise SystemExit(f"unhandled sub-byte vector address at {c_path.name}:{lno+1}: {ln}")
                        indent, dst, src, _sz = mm.groups()
                        lanes, elem = subbyte_defs[vtype]
                        if elem != "bool" or lanes > 64:
                            raise SystemExit(f"unhandled non-bool sub-byte vector bitcast at {c_path.name}:{lno+1}: {ln}")
                        if src == vname and dst in smap:
                            dtype = smap[dst]
                            terms = " | ".join(
                                f"(({src}.array[{i}] ? UINT64_C(1) : UINT64_C(0)) << {i})"
                                for i in range(lanes)
                            )
                            lines[lno] = f"{indent}{dst} = ({dtype})({terms});"
                            rewrites += 1
                        elif dst == vname and src in smap:
                            stmts = " ".join(
                                f"{dst}.array[{i}] = (((uint64_t){src} >> {i}) & UINT64_C(1)) != 0;"
                                for i in range(lanes)
                            )
                            lines[lno] = f"{indent}{{ {stmts} }}"
                            rewrites += 1
                        else:
                            raise SystemExit(f"unhandled bool vector memcpy at {c_path.name}:{lno+1}: {ln}")
            func_start = None

    if rewrites == 0:
        raise SystemExit(f"expected bool-vector bitcast rewrites in {c_path.name}, found 0")
    print(f"rewrote {rewrites} @Vector(N, bool) bitcasts in {c_path.name}")
    c_path.write_text("\n".join(lines) + "\n")
PY

mkdir -p /out/include/ghostty
cd /tmp/vt/include/ghostty
find . -type f -name "*.h" -exec install -D -m 0644 "{}" "/out/include/ghostty/{}" \;

cat > /out/zig_rt_shims.c << "EOF"
/*
 * Minimal C99 hardware-float conversion shims for Zig 0.16 C backend output.
 *
 * zig.h lowers float<->int and float<->double conversions through compiler-rt
 * helper names (__truncdfsf2, __extendsfdf2, __floatunsidf, etc.). On Linux
 * targets with mandatory hardware IEEE-754 floating point (such as aarch64),
 * system libgcc omits soft-float helpers from libgcc.a. Providing weak hidden
 * C99 definitions resolves them to single hardware instructions without
 * conflicting if a toolchain runtime already defines them.
 */
#include <stdint.h>

#if defined(__GNUC__) || defined(__clang__)
#define ZIG_RT_SHIM __attribute__((weak, visibility("hidden")))
#else
#define ZIG_RT_SHIM
#endif

ZIG_RT_SHIM float __truncdfsf2(double a) { return (float)a; }
ZIG_RT_SHIM double __extendsfdf2(float a) { return (double)a; }

ZIG_RT_SHIM int32_t __fixsfsi(float a) { return (int32_t)a; }
ZIG_RT_SHIM int64_t __fixsfdi(float a) { return (int64_t)a; }
ZIG_RT_SHIM uint32_t __fixunssfsi(float a) { return a <= 0.0f ? 0U : (uint32_t)a; }
ZIG_RT_SHIM uint64_t __fixunssfdi(float a) { return a <= 0.0f ? 0ULL : (uint64_t)a; }

ZIG_RT_SHIM int32_t __fixdfsi(double a) { return (int32_t)a; }
ZIG_RT_SHIM int64_t __fixdfdi(double a) { return (int64_t)a; }
ZIG_RT_SHIM uint32_t __fixunsdfsi(double a) { return a <= 0.0 ? 0U : (uint32_t)a; }
ZIG_RT_SHIM uint64_t __fixunsdfdi(double a) { return a <= 0.0 ? 0ULL : (uint64_t)a; }

ZIG_RT_SHIM float __floatsisf(int32_t a) { return (float)a; }
ZIG_RT_SHIM float __floatdisf(int64_t a) { return (float)a; }
ZIG_RT_SHIM float __floatunsisf(uint32_t a) { return (float)a; }
ZIG_RT_SHIM float __floatundisf(uint64_t a) { return (float)a; }

ZIG_RT_SHIM double __floatsidf(int32_t a) { return (double)a; }
ZIG_RT_SHIM double __floatdidf(int64_t a) { return (double)a; }
ZIG_RT_SHIM double __floatunsidf(uint32_t a) { return (double)a; }
ZIG_RT_SHIM double __floatundidf(uint64_t a) { return (double)a; }
EOF

echo "==> Verifying C99 compilation and live native ABI smoke test..."
HOST_ARCH="$(uname -m)"
if [[ "${HOST_ARCH}" == "aarch64" || "${HOST_ARCH}" == "arm64" ]]; then
    HOST_C="/out/ghostty_vt_aarch64_linux.c"
    CROSS_C="/out/ghostty_vt_x86_64_linux.c"
    CROSS_TARGET="x86_64-linux-gnu"
else
    HOST_C="/out/ghostty_vt_x86_64_linux.c"
    CROSS_C="/out/ghostty_vt_aarch64_linux.c"
    CROSS_TARGET="aarch64-linux-gnu"
fi

CFLAGS=(-std=c99 -O3 -fno-strict-aliasing -fwrapv -fPIC -Wno-unused-function -Wno-unused-variable -Wno-unused-but-set-variable -Wno-unused-label)
gcc "${CFLAGS[@]}" -I/out -c "${HOST_C}" -o /tmp/vt_host.o
gcc "${CFLAGS[@]}" -c /out/zig_rt_shims.c -o /tmp/shims_host.o
/opt/zig/zig cc -target "${CROSS_TARGET}" "${CFLAGS[@]}" -Wno-parentheses-equality -I/out -c "${CROSS_C}" -o /tmp/vt_cross.o

cat > /tmp/smoke.c << "EOF"
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <ghostty/vt.h>

static char reply_buf[256];
static size_t reply_len = 0;

static void on_write_pty(GhosttyTerminal terminal, void *userdata, const uint8_t *data, size_t len) {
    (void)terminal;
    (void)userdata;
    if (reply_len + len <= sizeof(reply_buf)) {
        memcpy(reply_buf + reply_len, data, len);
        reply_len += len;
    }
}

int main(void) {
    GhosttyTerminal term = NULL;
    assert(ghostty_terminal_new(NULL, &term, 80, 24) == GHOSTTY_SUCCESS);
    assert(ghostty_terminal_set(term, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (const void *)&on_write_pty) == GHOSTTY_SUCCESS);

    const char *input = "Hello world 12345 \x1b[32mC99 VT\x1b[0m!\r\nSecond\x1b[>4;2m\x1b[5n\x1b[6n";
    ghostty_terminal_vt_write(term, (const uint8_t *)input, strlen(input));

    bool mok = false;
    assert(ghostty_terminal_get(term, GHOSTTY_TERMINAL_DATA_MODIFY_OTHER_KEYS, &mok) == GHOSTTY_SUCCESS);
    assert(mok == true);
    assert(reply_len == 10 && memcmp(reply_buf, "\x1b[0n\x1b[2;7R", 10) == 0);

    bool kitty_enabled = true;
    assert(ghostty_build_info(GHOSTTY_BUILD_INFO_KITTY_GRAPHICS, &kitty_enabled) == GHOSTTY_SUCCESS);
    assert(kitty_enabled == false);

    bool simd_enabled = true;
    assert(ghostty_build_info(GHOSTTY_BUILD_INFO_SIMD, &simd_enabled) == GHOSTTY_SUCCESS);
    assert(simd_enabled == false);

    (void)ghostty_terminal_clear_screen(term);
    ghostty_terminal_free(term);
    printf("smoke ok: modifyOtherKeys=%d kitty=%d simd=%d\n", (int)mok, (int)kitty_enabled, (int)simd_enabled);
    return 0;
}
EOF

gcc -std=c99 -O3 -I/out/include /tmp/smoke.c /tmp/vt_host.o /tmp/shims_host.o -lm -o /tmp/smoke
/tmp/smoke

ZIG_ACTUAL_VER="$(/opt/zig/zig version)"
ZIG_ACTUAL_VER="${ZIG_ACTUAL_VER}" python3 - << "PY"
import hashlib
import json
import os
from pathlib import Path

repo = Path("/repo")
out = Path("/out")

vendor_meta = json.loads((repo / "vendor/libghostty-vt.vendor.json").read_text())
version_str = (repo / "vendor/libghostty-vt/VERSION").read_text().strip()

def sha256_file(path: Path) -> str:
    h = hashlib.sha256()
    with path.open("rb") as f:
        for chunk in iter(lambda: f.read(65536), b""):
            h.update(chunk)
    return h.hexdigest()

active_text_patch_prefixes = ("0002-", "0005-", "0006-", "0008-")
patch_dir = repo / "vendor/patches/libghostty-vt"
all_patches = []
active_text_patches = []
for patch_path in sorted(patch_dir.glob("*.patch")):
    entry = {
        "path": patch_path.relative_to(repo).as_posix(),
        "sha256": sha256_file(patch_path),
    }
    all_patches.append(entry)
    if patch_path.name.startswith(active_text_patch_prefixes):
        active_text_patches.append(entry)

generated_files = []
for file_path in sorted(p for p in out.rglob("*") if p.is_file() and p.name != "provenance.json"):
    generated_files.append({
        "path": file_path.relative_to(out).as_posix(),
        "sha256": sha256_file(file_path),
    })

provenance = {
    "source_commit": vendor_meta["source_commit"],
    "version": version_str,
    "generator": {
        "script": "scripts/regenerate_libghostty_vt_c.sh",
        "tier": 1,
        "backend": "zig-ofmt-c",
        "zig_version": os.environ["ZIG_ACTUAL_VER"],
        "optimize": "ReleaseFast",
        "simd": False,
        "kitty_graphics": False,
    },
    "targets": [
        "x86_64-linux-gnu",
        "aarch64-linux-gnu",
    ],
    "active_text_patches": active_text_patches,
    "all_vendored_patches": all_patches,
    "generated_files": generated_files,
}

(out / "provenance.json").write_text(json.dumps(provenance, indent=2) + "\n")
PY
'

rm -rf "${OUT_DIR}"
mkdir -p "${OUT_DIR}"
cp -R "${STAGE_DIR}/." "${OUT_DIR}/"
find "${OUT_DIR}" -type d -exec chmod 0755 "{}" +
find "${OUT_DIR}" -type f -exec chmod 0644 "{}" +

# Guardrail: refuse any binary artifacts in vendor/libghostty-vt-c/
if find "${OUT_DIR}" -type f \( -name '*.a' -o -name '*.so' -o -name '*.wasm' -o -name '*.o' -o -name '*.dylib' -o -name '*.lib' \) | grep -q .; then
    echo "error: binary artifact detected under ${OUT_DIR}" >&2
    exit 1
fi

echo "==> Successfully regenerated ${OUT_DIR}"
