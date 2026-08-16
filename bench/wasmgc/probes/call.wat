;; Probe E — the host crossing. The JS backend calls a DOM method directly; a
;; WasmGC module must cross wasm->JS for every one. cssom does four per
;; iteration, dom and streams similar.
(module
  (import "h" "sink" (func $sink (param i32) (result i32)))
  (func (export "loop") (param $n i32) (result i32)
    (local $i i32) (local $s i32)
    (block $d (loop $L
      (br_if $d (i32.ge_s (local.get $i) (local.get $n)))
      (local.set $s (i32.add (local.get $s) (call $sink (local.get $i))))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br $L)))
    (local.get $s)))
