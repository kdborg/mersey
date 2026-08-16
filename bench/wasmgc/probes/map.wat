;; Probe C — a keyed map, the primitive reconcile is actually bound by:
;;   private readonly nodes: Map<int32, Entry> = new Map<int32, Entry>();
;; with get / set / remove / entries() traffic on every render.
;;
;; This is the asymmetry that decides the question. JS gets V8's native Map,
;; written in C++/Torque and tuned for years. WasmGC has no map at all, so a
;; backend must emit one — this is open addressing with linear probing, i32
;; keys, struct-ref values, and a real rehash at 0.75 load, which is what such a
;; backend would generate.
(module
  (type $entry (struct (field $node (mut i32)) (field $v (mut i32))))
  (type $ints (array (mut i32)))
  (type $entries (array (mut (ref null $entry))))

  (global $keys (mut (ref null $ints)) (ref.null $ints))
  (global $state (mut (ref null $ints)) (ref.null $ints))  ;; 0 empty 1 live 2 tomb
  (global $vals (mut (ref null $entries)) (ref.null $entries))
  (global $cap (mut i32) (i32.const 0))
  (global $live (mut i32) (i32.const 0))
  (global $used (mut i32) (i32.const 0))                   ;; live + tombstones

  (func $alloc (param $c i32)
    (global.set $keys (array.new_default $ints (local.get $c)))
    (global.set $state (array.new_default $ints (local.get $c)))
    (global.set $vals (array.new_default $entries (local.get $c)))
    (global.set $cap (local.get $c))
    (global.set $live (i32.const 0))
    (global.set $used (i32.const 0)))

  (func (export "reset") (call $alloc (i32.const 16)))

  ;; Fibonacci hashing, then mask to the power-of-two capacity.
  (func $slot (param $k i32) (result i32)
    (i32.and (i32.shr_u (i32.mul (local.get $k) (i32.const 0x9E3779B1)) (i32.const 16))
             (i32.sub (global.get $cap) (i32.const 1))))

  ;; Index of the key, or -1.
  (func $find (param $k i32) (result i32)
    (local $i i32) (local $st i32) (local $probe i32)
    (local.set $i (call $slot (local.get $k)))
    (block $done
      (loop $L
        (local.set $st (array.get $ints (ref.as_non_null (global.get $state)) (local.get $i)))
        (br_if $done (i32.eqz (local.get $st)))                      ;; empty: absent
        (if (i32.and (i32.eq (local.get $st) (i32.const 1))
                     (i32.eq (array.get $ints (ref.as_non_null (global.get $keys)) (local.get $i))
                             (local.get $k)))
          (then (return (local.get $i))))
        (local.set $i (i32.and (i32.add (local.get $i) (i32.const 1))
                               (i32.sub (global.get $cap) (i32.const 1))))
        (local.set $probe (i32.add (local.get $probe) (i32.const 1)))
        (br_if $done (i32.ge_s (local.get $probe) (global.get $cap)))
        (br $L)))
    (i32.const -1))

  (func $grow
    (local $oldk (ref $ints)) (local $olds (ref $ints)) (local $oldv (ref $entries))
    (local $oldcap i32) (local $i i32) (local $j i32)
    (local.set $oldk (ref.as_non_null (global.get $keys)))
    (local.set $olds (ref.as_non_null (global.get $state)))
    (local.set $oldv (ref.as_non_null (global.get $vals)))
    (local.set $oldcap (global.get $cap))
    (call $alloc (i32.mul (local.get $oldcap) (i32.const 2)))
    (block $done
      (loop $L
        (br_if $done (i32.ge_s (local.get $i) (local.get $oldcap)))
        (if (i32.eq (array.get $ints (local.get $olds) (local.get $i)) (i32.const 1))
          (then
            (local.set $j (call $slot (array.get $ints (local.get $oldk) (local.get $i))))
            (block $placed
              (loop $P
                (br_if $placed (i32.eqz (array.get $ints (ref.as_non_null (global.get $state)) (local.get $j))))
                (local.set $j (i32.and (i32.add (local.get $j) (i32.const 1))
                                       (i32.sub (global.get $cap) (i32.const 1))))
                (br $P)))
            (array.set $ints (ref.as_non_null (global.get $keys)) (local.get $j)
              (array.get $ints (local.get $oldk) (local.get $i)))
            (array.set $ints (ref.as_non_null (global.get $state)) (local.get $j) (i32.const 1))
            (array.set $entries (ref.as_non_null (global.get $vals)) (local.get $j)
              (array.get $entries (local.get $oldv) (local.get $i)))
            (global.set $live (i32.add (global.get $live) (i32.const 1)))
            (global.set $used (i32.add (global.get $used) (i32.const 1)))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L))))

  (func (export "set") (param $k i32) (param $node i32) (param $v i32)
    (local $i i32)
    ;; grow at 0.75 load on used slots (live + tombstones)
    (if (i32.ge_s (i32.mul (global.get $used) (i32.const 4))
                  (i32.mul (global.get $cap) (i32.const 3)))
      (then (call $grow)))
    (local.set $i (call $find (local.get $k)))
    (if (i32.ge_s (local.get $i) (i32.const 0))
      (then
        (struct.set $entry $node
          (ref.as_non_null (array.get $entries (ref.as_non_null (global.get $vals)) (local.get $i)))
          (local.get $node))
        (struct.set $entry $v
          (ref.as_non_null (array.get $entries (ref.as_non_null (global.get $vals)) (local.get $i)))
          (local.get $v))
        (return)))
    (local.set $i (call $slot (local.get $k)))
    (block $placed
      (loop $P
        (br_if $placed (i32.ne (array.get $ints (ref.as_non_null (global.get $state)) (local.get $i))
                               (i32.const 1)))
        (local.set $i (i32.and (i32.add (local.get $i) (i32.const 1))
                               (i32.sub (global.get $cap) (i32.const 1))))
        (br $P)))
    (if (i32.eqz (array.get $ints (ref.as_non_null (global.get $state)) (local.get $i)))
      (then (global.set $used (i32.add (global.get $used) (i32.const 1)))))
    (array.set $ints (ref.as_non_null (global.get $keys)) (local.get $i) (local.get $k))
    (array.set $ints (ref.as_non_null (global.get $state)) (local.get $i) (i32.const 1))
    (array.set $entries (ref.as_non_null (global.get $vals)) (local.get $i)
      (struct.new $entry (local.get $node) (local.get $v)))
    (global.set $live (i32.add (global.get $live) (i32.const 1))))

  ;; Sum of the entry's two fields, or 0 when absent — a get plus two field reads.
  (func (export "get") (param $k i32) (result i32)
    (local $i i32) (local $e (ref null $entry))
    (local.set $i (call $find (local.get $k)))
    (if (i32.lt_s (local.get $i) (i32.const 0)) (then (return (i32.const 0))))
    (local.set $e (array.get $entries (ref.as_non_null (global.get $vals)) (local.get $i)))
    (i32.add (struct.get $entry $node (ref.as_non_null (local.get $e)))
             (struct.get $entry $v (ref.as_non_null (local.get $e)))))

  (func (export "remove") (param $k i32) (result i32)
    (local $i i32)
    (local.set $i (call $find (local.get $k)))
    (if (i32.lt_s (local.get $i) (i32.const 0)) (then (return (i32.const 0))))
    (array.set $ints (ref.as_non_null (global.get $state)) (local.get $i) (i32.const 2))
    (array.set $entries (ref.as_non_null (global.get $vals)) (local.get $i) (ref.null $entry))
    (global.set $live (i32.sub (global.get $live) (i32.const 1)))
    (i32.const 1))

  ;; entries() — reconcile walks the whole map every render.
  (func (export "iterate") (result i32)
    (local $i i32) (local $s i32) (local $e (ref null $entry))
    (block $done
      (loop $L
        (br_if $done (i32.ge_s (local.get $i) (global.get $cap)))
        (if (i32.eq (array.get $ints (ref.as_non_null (global.get $state)) (local.get $i)) (i32.const 1))
          (then
            (local.set $e (array.get $entries (ref.as_non_null (global.get $vals)) (local.get $i)))
            (local.set $s (i32.add (local.get $s)
              (i32.add (array.get $ints (ref.as_non_null (global.get $keys)) (local.get $i))
                       (struct.get $entry $v (ref.as_non_null (local.get $e))))))))
        (local.set $i (i32.add (local.get $i) (i32.const 1)))
        (br $L)))
    (local.get $s))
)
