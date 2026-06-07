;; Deterministic compute payload for cross-platform benchmarking.
;; Computes fibonacci(30) ten times and discards the result.
;; This produces enough CPU work (~1.6M recursive calls) that microsecond-
;; level startup overhead does not dominate the wall-clock measurement,
;; while still being short enough to run hundreds of times under Hyperfine.
(module
  (func $fib (param $n i32) (result i32)
    (local $a i32)
    (local $b i32)
    (if (i32.lt_s (local.get $n) (i32.const 2))
      (then (return (local.get $n)))
    )
    (local.set $a (call $fib (i32.sub (local.get $n) (i32.const 1))))
    (local.set $b (call $fib (i32.sub (local.get $n) (i32.const 2))))
    (i32.add (local.get $a) (local.get $b))
  )

  (func (export "_start")
    (local $i i32)
    (local.set $i (i32.const 0))
    (loop $repeat
      (drop (call $fib (i32.const 30)))
      (local.set $i (i32.add (local.get $i) (i32.const 1)))
      (br_if $repeat (i32.lt_s (local.get $i) (i32.const 10)))
    )
  )
)
