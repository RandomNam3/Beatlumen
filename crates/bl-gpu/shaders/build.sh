#!/bin/sh
# Compiles the compute shaders to SPIR-V (Vulkan 1.0) and checks the
# results. The .spv files are committed, so building the crate needs no
# shader compiler; run this after changing a .comp file.
# Needs glslangValidator and spirv-val (Debian/Ubuntu: glslang-tools,
# spirv-tools).
set -eu
cd "$(dirname "$0")"

compile() {
    out="$1"
    shift
    glslangValidator -V --target-env vulkan1.0 -Os "$@" -o "$out.spv" >/dev/null
    spirv-val --target-env vulkan1.0 "$out.spv"
    echo "$out.spv"
}

for fmt in F32 F16 Q8_0 Q5_0 Q5_1 Q4_0 Q4_1; do
    name=$(echo "$fmt" | tr 'A-Z' 'a-z')
    compile "matmul_$name" -DW_"$fmt" matmul.comp
done
compile layer_norm layer_norm.comp
compile attention attention.comp
compile im2col im2col.comp
