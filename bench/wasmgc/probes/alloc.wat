;; Probe A v2 — same as v1 but the transient case *escapes*, so neither engine
;; can scalar-replace the allocation away. Each new struct is parked in a
;; mutable global (and a ring buffer, for the "short-lived but real" case) so it
;; must be built on the heap, then becomes garbage as the next one replaces it.
(module
  (type $row (struct (field $id (mut i32)) (field $v (mut i32))))
  (type $rows (array (mut (ref null $row))))

  (global $sink (mut (ref null $row)) (ref.null $row))

  ;; n allocations that genuinely happen and then die.
  (func (export "transient") (param $n i32) (result i32)
    (local $i i32) (local $s i32) (local $r (ref $row))
    (block $done
      (loop $L
        (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $r
          (struct.new $row (local.get $i) (i32.mul (local.get $i) (i32.const 2))))
        (global.set $sink (local.get $r))
        (local.set $s
          (i32.add (local.get $s)
            (i32.add (struct.get $row $id (local.get $r))
                     (struct.get $row $v (local.get $r)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L)))
    (local.get $s))

  ;; A bounded live set — `window` objects alive at once, the rest garbage.
  ;; This is the shape a reconciler actually has: a working set that survives a
  ;; render and is dropped on the next one.
  (func (export "ring") (param $n i32) (param $window i32) (result i32)
    (local $i i32) (local $s i32) (local $a (ref $rows)) (local $r (ref $row))
    (local.set $a (array.new_default $rows (local.get $window)))
    (block $done
      (loop $L
        (br_if $done (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $r
          (struct.new $row (local.get $i) (i32.mul (local.get $i) (i32.const 2))))
        (array.set $rows (local.get $a)
          (i32.rem_s (local.get $i) (local.get $window)) (local.get $r))
        (local.set $s
          (i32.add (local.get $s)
            (i32.add (struct.get $row $id (local.get $r))
                     (struct.get $row $v (local.get $r)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L)))
    (local.get $s))

  ;; Everything retained, then walked.
  (func (export "retained") (param $n i32) (result i32)
    (local $i i32) (local $s i32) (local $a (ref $rows)) (local $r (ref $row))
    (local.set $a (array.new_default $rows (local.get $n)))
    (block $d1
      (loop $L1
        (br_if $d1 (i32.ge_s (local.get $i) (local.get $n)))
        (array.set $rows (local.get $a) (local.get $i)
          (struct.new $row (local.get $i) (i32.mul (local.get $i) (i32.const 2))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L1)))
    (local.set $i (i32.const 0))
    (block $d2
      (loop $L2
        (br_if $d2 (i32.ge_s (local.get $i) (local.get $n)))
        (local.set $r (ref.as_non_null (array.get $rows (local.get $a) (local.get $i))))
        (local.set $s
          (i32.add (local.get $s)
            (i32.add (struct.get $row $id (local.get $r))
                     (struct.get $row $v (local.get $r)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L2)))
    (local.get $s))
)
