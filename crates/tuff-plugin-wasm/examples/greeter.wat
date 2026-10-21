;; greeter: an example runtime plugin, hand-written in WebAssembly text
;; against the `plugin` world in crates/tuff-plugin-wasm/wit/ (the canonical
;; ABI, as wit-bindgen would generate it).
;;
;; Build it with any WAT assembler (`wat2wasm greeter.wat`, or the `wat`
;; crate), drop greeter.wasm into the server's plugins/ directory and run
;; `/tuff plugin load greeter`. It shows:
;;
;;   /greet         replies to the player, remembering per player whether
;;                  it has greeted them before (per-player state)
;;   /greet later   also schedules a broadcast 100 ticks later (a timer)
;;   joins          broadcasts "<name> joined; say hello!" (an event)
;;   enable/disable broadcasts when the plugin goes live and goes away
;;
;; Everything it registers disappears with `/tuff plugin unload greeter`.
(module
  ;; The manifest the server reads before running any code.
  (@custom "tuff-plugin" "id = greeter\nversion = 1.0.0\napi = 0\ndescription = Greets players and remembers who it has greeted\n")

  ;; Host functions. Strings are (pointer, length) in this module's
  ;; memory; a trailing pointer says where to write a result.
  (import "tuff:plugin/host@0.1.0" "log" (func $log (param i32 i32 i32)))
  (import "tuff:plugin/host@0.1.0" "register-command" (func $register-command (param i32 i32 i32)))
  (import "tuff:plugin/host@0.1.0" "subscribe" (func $subscribe (param i32 i32 i32)))
  (import "tuff:plugin/host@0.1.0" "reply" (func $reply (param i32 i32 i32)))
  (import "tuff:plugin/host@0.1.0" "broadcast" (func $broadcast (param i32 i32 i32)))
  (import "tuff:plugin/host@0.1.0" "schedule" (func $schedule (param i64 i32 i32)))
  (import "tuff:plugin/host@0.1.0" "state-get" (func $state-get (param i32 i32 i32)))
  (import "tuff:plugin/host@0.1.0" "state-set" (func $state-set (param i32 i32 i32 i32 i32 i32)))

  (memory (export "memory") 1)

  (data (i32.const 16) "greet")
  (data (i32.const 24) "later")
  (data (i32.const 32) "seen")
  (data (i32.const 48) "Hello! Nice to meet you.")
  (data (i32.const 80) "Welcome back!")
  (data (i32.const 96) "Reminder: say hi to someone today.")
  (data (i32.const 160) "greeter is ready")
  (data (i32.const 192) "greeter is shutting down")
  (data (i32.const 224) " joined; say hello!")
  (data (i32.const 256) "I will remind everyone in 5 seconds.")
  ;; The root's children: node 1, as a list<u16>.
  (data (i32.const 304) "\01\00")

  ;; Where results go: 384 for this module's own, 400 for the host's.
  ;; The command tree is built at 512, a join message at 2048.

  ;; The host asks for memory here to pass arguments in. A bump allocator
  ;; is enough: every callback below resets it when it returns.
  (global $heap (mut i32) (i32.const 4096))
  (func (export "cabi_realloc") (param i32 i32 i32 i32) (result i32)
    (local $ptr i32)
    (local.set $ptr
      (i32.and
        (i32.add (global.get $heap) (i32.sub (local.get 2) (i32.const 1)))
        (i32.sub (i32.const 0) (local.get 2))))
    (global.set $heap (i32.add (local.get $ptr) (local.get 3)))
    (if (i32.gt_u (global.get $heap) (i32.mul (memory.size) (i32.const 65536)))
      (then (drop (memory.grow (i32.const 1)))))
    (local.get $ptr))
  (func $free_all
    (global.set $heap (i32.const 4096)))

  ;; A literal command-node (96 bytes, see the canonical ABI) at $at whose
  ;; input ends in handler $handler.
  (func $node (param $at i32) (param $name i32) (param $len i32)
              (param $children i32) (param $count i32) (param $handler i32)
    (i32.store (local.get $at) (local.get $name))
    (i32.store offset=4 (local.get $at) (local.get $len))
    (i32.store offset=56 (local.get $at) (local.get $children))
    (i32.store offset=60 (local.get $at) (local.get $count))
    (i32.store8 offset=64 (local.get $at) (i32.const 1))
    (i32.store offset=68 (local.get $at) (local.get $handler)))

  ;; The only place commands and events can be claimed: /greet (handler
  ;; 0) and /greet later (handler 1), and joins (event-kinds bit 0).
  (func (export "tuff:plugin/guest@0.1.0#register") (result i32)
    (call $node (i32.const 512) (i32.const 16) (i32.const 5) (i32.const 304) (i32.const 1) (i32.const 0))
    (call $node (i32.const 608) (i32.const 24) (i32.const 5) (i32.const 0) (i32.const 0) (i32.const 1))
    (call $register-command (i32.const 512) (i32.const 2) (i32.const 400))
    (call $subscribe (i32.const 1) (i32.const 0) (i32.const 400))
    (i32.store (i32.const 384) (i32.const 0))
    (i32.const 384))

  (func (export "tuff:plugin/guest@0.1.0#enable")
    (call $log (i32.const 2) (i32.const 160) (i32.const 16))
    (call $broadcast (i32.const 160) (i32.const 16) (i32.const 400)))

  (func (export "tuff:plugin/guest@0.1.0#disable")
    (call $broadcast (i32.const 192) (i32.const 24) (i32.const 400)))

  ;; An invocation: its handler, its sender (an option<player>), its
  ;; arguments and the line.
  (func (export "tuff:plugin/guest@0.1.0#run-command")
        (param $handler i32) (param i32 i64 i64 i32 i32 i32 i32 i32 i32) (result i32)
    ;; state-get answers result<option<list<u8>>, string>: the option's
    ;; discriminant is at 404.
    (call $state-get (i32.const 32) (i32.const 4) (i32.const 400))
    (if (i32.load8_u (i32.const 404))
      (then
        (call $reply (i32.const 80) (i32.const 13) (i32.const 400)))
      (else
        (call $reply (i32.const 48) (i32.const 24) (i32.const 400))
        (call $state-set (i32.const 32) (i32.const 4) (i32.const 1) (i32.const 32) (i32.const 1) (i32.const 400))))
    (if (i32.eq (local.get $handler) (i32.const 1))
      (then
        (call $schedule (i64.const 100) (i32.const 1) (i32.const 400))
        (call $reply (i32.const 256) (i32.const 36) (i32.const 400))))
    (call $free_all)
    ;; ok(1)
    (i32.store (i32.const 384) (i32.const 0))
    (i32.store (i32.const 388) (i32.const 1))
    (i32.const 384))

  ;; The event arrives as a pointer: its case tag in the first byte and
  ;; the payload at offset 8. Case 0 is a join of a player: their uuid
  ;; (16 bytes), then their name's pointer and length.
  (func (export "tuff:plugin/guest@0.1.0#on-event") (param $ev i32)
    (local $len i32)
    (if (i32.eqz (i32.load8_u (local.get $ev)))
      (then
        (local.set $len (i32.load offset=28 (local.get $ev)))
        (memory.copy (i32.const 2048) (i32.load offset=24 (local.get $ev)) (local.get $len))
        (memory.copy (i32.add (i32.const 2048) (local.get $len)) (i32.const 224) (i32.const 19))
        (call $broadcast (i32.const 2048) (i32.add (local.get $len) (i32.const 19)) (i32.const 400))))
    (call $free_all))

  (func (export "tuff:plugin/guest@0.1.0#on-timer") (param $token i32)
    (call $broadcast (i32.const 96) (i32.const 34) (i32.const 400))))
