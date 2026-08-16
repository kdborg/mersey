;; Probe D — strings, the thing WasmGC does not have.
;;
;; reconcile builds `row ${r.id} v${r.v}` on every changed row, and cssom builds
;; two template strings per iteration, each of which then crosses to the DOM. A
;; WasmGC backend has to construct those in a (array i16) and then turn them
;; into a real JS string for the host — the JS backend just... has a string.
;;
;; This uses the JS String Builtins proposal (`wasm:js-string`), which is the
;; only practical answer since stringref was withdrawn.
(module
  (type $chars (array (mut i16)))

  (import "wasm:js-string" "fromCharCodeArray"
    (func $fromCharCodeArray (param (ref null $chars) i32 i32) (result (ref extern))))

  ;; Write the decimal digits of $v into $a at $p; returns the new position.
  (func $digits (param $a (ref $chars)) (param $p i32) (param $v i32) (result i32)
    (local $start i32) (local $end i32) (local $t i32)
    (local.set $start (local.get $p))
    (if (i32.eqz (local.get $v))
      (then
        (array.set $chars (local.get $a) (local.get $p) (i32.const 48))
        (return (i32.add (local.get $p) (i32.const 1)))))
    (block $done
      (loop $L
        (br_if $done (i32.eqz (local.get $v)))
        (array.set $chars (local.get $a) (local.get $p)
          (i32.add (i32.const 48) (i32.rem_u (local.get $v) (i32.const 10))))
        (local.set $p (i32.add (local.get $p) (i32.const 1)))
        (local.set $v (i32.div_u (local.get $v) (i32.const 10)))
        (br $L)))
    ;; reverse the digits just written
    (local.set $end (i32.sub (local.get $p) (i32.const 1)))
    (block $rdone
      (loop $R
        (br_if $rdone (i32.ge_s (local.get $start) (local.get $end)))
        (local.set $t (array.get_u $chars (local.get $a) (local.get $start)))
        (array.set $chars (local.get $a) (local.get $start)
          (array.get_u $chars (local.get $a) (local.get $end)))
        (array.set $chars (local.get $a) (local.get $end) (local.get $t))
        (local.set $start (i32.add (local.get $start) (i32.const 1)))
        (local.set $end (i32.sub (local.get $end) (i32.const 1)))
        (br $R)))
    (local.get $p))

  ;; Build "row <id> v<v>" and hand it to the host as a real JS string.
  (func (export "rowLabel") (param $id i32) (param $v i32) (result (ref extern))
    (local $a (ref $chars)) (local $p i32)
    (local.set $a (array.new_default $chars (i32.const 32)))
    (array.set $chars (local.get $a) (i32.const 0) (i32.const 114)) ;; r
    (array.set $chars (local.get $a) (i32.const 1) (i32.const 111)) ;; o
    (array.set $chars (local.get $a) (i32.const 2) (i32.const 119)) ;; w
    (array.set $chars (local.get $a) (i32.const 3) (i32.const 32))  ;; space
    (local.set $p (call $digits (local.get $a) (i32.const 4) (local.get $id)))
    (array.set $chars (local.get $a) (local.get $p) (i32.const 32))  ;; space
    (local.set $p (i32.add (local.get $p) (i32.const 1)))
    (array.set $chars (local.get $a) (local.get $p) (i32.const 118)) ;; v
    (local.set $p (i32.add (local.get $p) (i32.const 1)))
    (local.set $p (call $digits (local.get $a) (local.get $p) (local.get $v)))
    (call $fromCharCodeArray (local.get $a) (i32.const 0) (local.get $p)))

  ;; The same construction WITHOUT the handoff, to separate the two costs:
  ;; how much is building the characters, how much is becoming a JS string.
  (func (export "rowLabelNoHandoff") (param $id i32) (param $v i32) (result i32)
    (local $a (ref $chars)) (local $p i32)
    (local.set $a (array.new_default $chars (i32.const 32)))
    (array.set $chars (local.get $a) (i32.const 0) (i32.const 114))
    (array.set $chars (local.get $a) (i32.const 1) (i32.const 111))
    (array.set $chars (local.get $a) (i32.const 2) (i32.const 119))
    (array.set $chars (local.get $a) (i32.const 3) (i32.const 32))
    (local.set $p (call $digits (local.get $a) (i32.const 4) (local.get $id)))
    (array.set $chars (local.get $a) (local.get $p) (i32.const 32))
    (local.set $p (i32.add (local.get $p) (i32.const 1)))
    (array.set $chars (local.get $a) (local.get $p) (i32.const 118))
    (local.set $p (i32.add (local.get $p) (i32.const 1)))
    (local.set $p (call $digits (local.get $a) (local.get $p) (local.get $v)))
    (local.get $p))
)
