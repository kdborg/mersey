;; Probe B — a growable int32 array, which reconcile leans on hard: every op
;; pushes four ints, plus strs/liveNodes/drop/newRows/newRefs.
;;
;; WasmGC arrays are fixed-length, so "push" is not a primitive — a backend has
;; to emit the doubling itself (allocate, array.copy, swap). JS arrays grow
;; natively on a backing store V8 has spent years on. That asymmetry is the
;; point of this probe.
(module
  (type $ints (array (mut i32)))

  (global $buf (mut (ref null $ints)) (ref.null $ints))
  (global $len (mut i32) (i32.const 0))
  (global $cap (mut i32) (i32.const 0))

  (func $reset
    (global.set $buf (array.new_default $ints (i32.const 8)))
    (global.set $len (i32.const 0))
    (global.set $cap (i32.const 8)))

  ;; push with amortised doubling — the code a backend would emit per append.
  (func $push (param $v i32)
    (local $n (ref $ints))
    (if (i32.ge_s (global.get $len) (global.get $cap))
      (then
        (local.set $n (array.new_default $ints (i32.mul (global.get $cap) (i32.const 2))))
        (array.copy $ints $ints
          (local.get $n) (i32.const 0)
          (ref.as_non_null (global.get $buf)) (i32.const 0)
          (global.get $len))
        (global.set $buf (local.get $n))
        (global.set $cap (i32.mul (global.get $cap) (i32.const 2)))))
    (array.set $ints (ref.as_non_null (global.get $buf)) (global.get $len) (local.get $v))
    (global.set $len (i32.add (global.get $len) (i32.const 1))))

  ;; Append n values, then walk the whole thing and sum.
  (func (export "build") (param $n i32) (result i32)
    (local $i i32) (local $s i32)
    (call $reset)
    (block $d1
      (loop $L1
        (br_if $d1 (i32.ge_s (local.get $i) (local.get $n)))
        (call $push (local.get $i))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L1)))
    (local.set $i (i32.const 0))
    (block $d2
      (loop $L2
        (br_if $d2 (i32.ge_s (local.get $i) (global.get $len)))
        (local.set $s (i32.add (local.get $s)
          (array.get $ints (ref.as_non_null (global.get $buf)) (local.get $i))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L2)))
    (local.get $s))

  ;; The same, but many small arrays rather than one big one — reconcile builds
  ;; a fresh op buffer per render, not one that grows forever.
  (func (export "churn") (param $rounds i32) (param $each i32) (result i32)
    (local $r i32) (local $i i32) (local $s i32)
    (block $dr
      (loop $LR
        (br_if $dr (i32.ge_s (local.get $r) (local.get $rounds)))
        (call $reset)
        (local.set $i (i32.const 0))
        (block $d1
          (loop $L1
            (br_if $d1 (i32.ge_s (local.get $i) (local.get $each)))
            (call $push (local.get $i))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br $L1)))
        (local.set $i (i32.const 0))
        (block $d2
          (loop $L2
            (br_if $d2 (i32.ge_s (local.get $i) (global.get $len)))
            (local.set $s (i32.add (local.get $s)
              (array.get $ints (ref.as_non_null (global.get $buf)) (local.get $i))))
            (local.set $i (i32.add (local.get $i) (i32.const 1)))
            (br $L2)))
        (local.set $r (i32.add (local.get $r) (i32.const 1)))
        (br $LR)))
    (local.get $s))
)
