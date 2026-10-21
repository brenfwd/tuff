// SPDX-License-Identifier: MIT
// Coroutines on a single-threaded executor: the host is a wasm interpreter
// without threads, so everything that waits is a coroutine suspended until
// one of the plugin's callbacks resumes it (a timer, for now).
//
//     tuff::Task greet_later(tuff::Ctx ctx) {
//         co_await tuff::sleep(40);
//         ctx.reply("two seconds later");
//     }
//
// A Task starts at once and runs until its first co_await, like the Rust
// SDK's spawn; an Async<T> starts when awaited and gives back a T, to
// split waiting work into functions. after() and every() run a callable
// later or repeatedly. Timer tokens from timer_base up are the executor's;
// TUFF_PLUGIN hands the rest to on_timer.
#ifndef TUFF_TASK_HPP
#define TUFF_TASK_HPP

#include <coroutine>

namespace tuff {

namespace detail {
struct FrameMemory {
    static void *operator new(size_t size) noexcept { return malloc(size); }
    static void operator delete(void *frame) noexcept { free(frame); }
};
} // namespace detail

// A detached coroutine: starts at once and frees itself when it ends.
struct Task {
    struct promise_type : detail::FrameMemory {
        Task get_return_object() noexcept { return {}; }
        static Task get_return_object_on_allocation_failure() noexcept { return {}; }
        std::suspend_never initial_suspend() noexcept { return {}; }
        std::suspend_never final_suspend() noexcept { return {}; }
        void return_void() noexcept {}
        void unhandled_exception() noexcept {}
    };
};

namespace detail {

struct Waiting {
    uint32_t token;
    std::coroutine_handle<> task;
};

inline Registry<Waiting> waiting;
inline uint32_t next_timer;

// Resumes the task waiting for `token`; false for the plugin's own tokens.
inline bool timer(uint32_t token) {
    if (token < timer_base) {
        return false;
    }
    for (size_t i = 0; i < waiting.size(); i++) {
        if (waiting[i].token == token) {
            std::coroutine_handle<> task = waiting[i].task;
            waiting.swap_remove(i);
            task.resume();
            break;
        }
    }
    return true;
}

template <class P> struct Continue {
    bool await_ready() noexcept { return false; }
    std::coroutine_handle<> await_suspend(std::coroutine_handle<P> done) noexcept {
        std::coroutine_handle<> next = done.promise().continuation;
        return next ? next : std::noop_coroutine();
    }
    void await_resume() noexcept {}
};

template <class T> struct AsyncResult {
    Option<T> value;
    void return_value(T v) noexcept { value = Option<T>(move(v)); }
    T take() { return value ? move(*value) : T(); }
};

template <> struct AsyncResult<void> {
    void return_void() noexcept {}
    void take() {}
};

} // namespace detail

// Waits `ticks` server ticks (20 per second); co_await gives false at once
// when the host refused the timer (while registering, or past its limit).
class Wait {
  public:
    explicit Wait(uint64_t ticks) : ticks_(ticks) {}
    bool await_ready() const noexcept { return false; }
    bool await_suspend(std::coroutine_handle<> task) noexcept {
        detail::next_timer = (detail::next_timer + 1) % timer_base;
        uint32_t token = timer_base + detail::next_timer;
        if (!raw::tuff_schedule(ticks_, token) || !detail::waiting.push({token, task})) {
            refused_ = true;
            return false;
        }
        return true;
    }
    bool await_resume() const noexcept { return !refused_; }

  private:
    uint64_t ticks_;
    bool refused_ = false;
};

inline Wait sleep(uint64_t ticks) { return Wait(ticks); }

// A coroutine that runs when awaited and gives a T (T() if its frame could
// not be allocated).
template <class T = void> class Async {
  public:
    struct promise_type : detail::FrameMemory, detail::AsyncResult<T> {
        std::coroutine_handle<> continuation;
        Async get_return_object() noexcept {
            return Async(std::coroutine_handle<promise_type>::from_promise(*this));
        }
        static Async get_return_object_on_allocation_failure() noexcept { return Async(nullptr); }
        std::suspend_always initial_suspend() noexcept { return {}; }
        detail::Continue<promise_type> final_suspend() noexcept { return {}; }
        void unhandled_exception() noexcept {}
    };

    explicit Async(std::coroutine_handle<promise_type> frame) : frame_(frame) {}
    Async(Async &&other) : frame_(other.frame_) { other.frame_ = nullptr; }
    Async(const Async &) = delete;
    ~Async() {
        if (frame_) {
            frame_.destroy();
        }
    }

    bool await_ready() const noexcept { return !frame_; }
    std::coroutine_handle<> await_suspend(std::coroutine_handle<> caller) noexcept {
        frame_.promise().continuation = caller;
        return frame_;
    }
    T await_resume() {
        if constexpr (same_type<T, void>) {
            return;
        } else {
            return frame_ ? frame_.promise().take() : T();
        }
    }

  private:
    std::coroutine_handle<promise_type> frame_;
};

// Runs an Async to its end, detached.
inline Task spawn(Async<> work) { co_await work; }

// A scheduled run; cancel() stops it (copies share the flag).
class Repeat {
  public:
    Repeat() : state_(static_cast<State *>(malloc(sizeof(State)))) {
        if (state_) {
            state_->cancelled = false;
            state_->refs = 1;
        }
    }
    Repeat(const Repeat &other) : state_(other.state_) {
        if (state_) {
            state_->refs++;
        }
    }
    Repeat &operator=(const Repeat &) = delete;
    ~Repeat() {
        if (state_ && --state_->refs == 0) {
            free(state_);
        }
    }
    void cancel() const {
        if (state_) {
            state_->cancelled = true;
        }
    }
    bool cancelled() const { return !state_ || state_->cancelled; }

  private:
    struct State {
        bool cancelled;
        int refs;
    };
    State *state_;
};

namespace detail {

inline bool enabled_now;
inline Registry<Function<void()>> deferred;

// The host refuses timers while registering: what is scheduled then starts
// when the plugin is enabled.
inline void start(Function<void()> go) {
    if (enabled_now) {
        go();
    } else {
        deferred.push(move(go));
    }
}

inline void enabled() {
    enabled_now = true;
    for (Function<void()> &go : deferred) {
        go();
    }
    deferred.clear();
}

inline void disabled() { enabled_now = false; }

inline Task run_after(uint64_t ticks, Function<void()> f, Repeat repeat) {
    if (co_await sleep(ticks) && !repeat.cancelled()) {
        f();
    }
}

inline Task run_every(uint64_t ticks, Function<void()> f, Repeat repeat) {
    while (co_await sleep(ticks ? ticks : 1) && !repeat.cancelled()) {
        f();
    }
}

} // namespace detail

// Runs `f` once after `ticks` ticks.
template <class F> Repeat after(uint64_t ticks, F f) {
    Repeat repeat;
    detail::start([ticks, f = Function<void()>(move(f)), repeat]() mutable {
        detail::run_after(ticks, move(f), repeat);
    });
    return repeat;
}

// Runs `f` every `ticks` ticks (at least 1) until cancelled.
template <class F> Repeat every(uint64_t ticks, F f) {
    Repeat repeat;
    detail::start([ticks, f = Function<void()>(move(f)), repeat]() mutable {
        detail::run_every(ticks, move(f), repeat);
    });
    return repeat;
}

} // namespace tuff

#endif
