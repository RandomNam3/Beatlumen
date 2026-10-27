# The prebuilt Rust standard library for wasm32-unknown-emscripten refers to
# the WebAssembly exception tag of C++ (`__cpp_exception`) in its unwinding
# paths. The core is built with panic=abort, so those paths never run, but
# the tag must exist to link with Qt (which uses JavaScript based exceptions).
	.globl	__cpp_exception
	.tagtype	__cpp_exception i32
__cpp_exception:
