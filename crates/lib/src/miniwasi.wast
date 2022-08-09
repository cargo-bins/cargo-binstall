(module
 (import "wasi_snapshot_preview1" "proc_exit" (func $exit (param i32)))
 (memory $0 0)
 (export "memory" (memory $0))
 (export "_start" (func $0))
 (func $0
  (call $exit (i32.const 0))
  (unreachable)
 )
)
