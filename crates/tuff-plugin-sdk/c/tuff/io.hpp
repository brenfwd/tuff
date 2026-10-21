// SPDX-License-Identifier: MIT
// Capability calls as awaitables: HTTP fetches and file operations start
// when awaited and resume the coroutine from the host's io callback.
//
//     tuff::Task show_ip(tuff::Ctx ctx) {
//         auto reply = co_await tuff::http::get("https://example.com/ip").timeout(5000);
//         if (!reply) {
//             ctx.reply("fetch refused");
//             co_return;
//         }
//         ctx.reply(reply->body.view());
//     }
//
//     auto saved = co_await tuff::files::write("notes.txt", "hello");
//     auto data = co_await tuff::files::read("notes.txt");
//
//     auto sum = co_await tuff::background("sum", input);  // runs P::background off the tick thread
//
// Every awaitable gives an Outcome: the value, or why the host refused or
// failed the operation (Denied). Sockets, listeners and routes are not
// awaitable; their io events reach the plugin's optional
// `static void on_io(const tuff::IoEvent *ev)`.
#ifndef TUFF_IO_HPP
#define TUFF_IO_HPP

namespace tuff {

using IoEvent = raw::exports_tuff_plugin_capability_events_io_event_t;

// A value or the error E.
template <class T, class E> class Outcome {
  public:
    Outcome() : error_() {}
    Outcome(T value) : value_(move(value)) {}
    Outcome(E error) : error_(move(error)) {}
    explicit operator bool() const { return static_cast<bool>(value_); }
    T &operator*() { return *value_; }
    const T &operator*() const { return *value_; }
    T *operator->() { return &*value_; }
    const T *operator->() const { return &*value_; }
    const E &error() const { return error_; }

  private:
    Option<T> value_;
    E error_{};
};

struct Unit {};

// Owned bytes.
class Bytes {
  public:
    Bytes() : b_{nullptr, 0} {}
    static Bytes adopt(raw::plugin_list_u8_t owned) {
        Bytes out;
        out.b_ = owned;
        return out;
    }
    static Bytes copy(const void *data, size_t len) {
        Bytes out;
        if (len) {
            out.b_.ptr = static_cast<uint8_t *>(malloc(len));
            if (out.b_.ptr) {
                memcpy(out.b_.ptr, data, len);
                out.b_.len = len;
            }
        }
        return out;
    }
    static Bytes copy(Str text) { return copy(text.ptr, text.len); }
    Bytes(Bytes &&other) : b_(other.b_) { other.b_ = {nullptr, 0}; }
    Bytes &operator=(Bytes &&other) {
        if (this != &other) {
            drop();
            b_ = other.b_;
            other.b_ = {nullptr, 0};
        }
        return *this;
    }
    Bytes(const Bytes &other) : Bytes(copy(other.b_.ptr, other.b_.len)) {}
    ~Bytes() { drop(); }

    const uint8_t *data() const { return b_.ptr; }
    size_t len() const { return b_.len; }
    // As text, for bodies that are UTF-8.
    Str view() const { return Str{b_.ptr, b_.len}; }
    raw::plugin_list_u8_t *raw() { return &b_; }

  private:
    void drop() {
        if (b_.len) {
            raw::plugin_list_u8_free(&b_);
        }
        b_ = {nullptr, 0};
    }

    raw::plugin_list_u8_t b_;
};

namespace detail {

// A growable owned list.
template <class T> class Vec {
  public:
    Vec() = default;
    Vec(Vec &&other) : items_(other.items_), len_(other.len_), cap_(other.cap_) {
        other.items_ = nullptr;
        other.len_ = other.cap_ = 0;
    }
    Vec &operator=(Vec &&other) {
        if (this != &other) {
            drop();
            items_ = other.items_;
            len_ = other.len_;
            cap_ = other.cap_;
            other.items_ = nullptr;
            other.len_ = other.cap_ = 0;
        }
        return *this;
    }
    Vec(const Vec &) = delete;
    ~Vec() { drop(); }

    bool push(T value) {
        if (len_ == cap_) {
            size_t cap = cap_ ? cap_ * 2 : 4;
            T *grown = static_cast<T *>(malloc(cap * sizeof(T)));
            if (!grown) {
                return false;
            }
            for (size_t i = 0; i < len_; i++) {
                new (&grown[i]) T(move(items_[i]));
                items_[i].~T();
            }
            free(items_);
            items_ = grown;
            cap_ = cap;
        }
        new (&items_[len_++]) T(move(value));
        return true;
    }
    size_t size() const { return len_; }
    T &operator[](size_t at) { return items_[at]; }
    const T &operator[](size_t at) const { return items_[at]; }
    T *begin() { return items_; }
    T *end() { return items_ + len_; }
    const T *begin() const { return items_; }
    const T *end() const { return items_ + len_; }

  private:
    void drop() {
        for (size_t i = 0; i < len_; i++) {
            items_[i].~T();
        }
        free(items_);
        items_ = nullptr;
        len_ = cap_ = 0;
    }

    T *items_ = nullptr;
    size_t len_ = 0;
    size_t cap_ = 0;
};

} // namespace detail

// Why the host refused or failed an operation; `tag` is one of the
// TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_* values, `text` its message when it
// has one and `port` the reserved port.
struct Denied {
    uint8_t tag = TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_FAILED;
    String text;
    uint16_t port = 0;

    bool timed_out() const { return tag == TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_TIMED_OUT; }
    bool not_found() const { return tag == TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_NOT_FOUND; }
    bool not_granted() const {
        return tag == TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_NOT_GRANTED ||
               tag == TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_NOT_REQUESTED;
    }

    static Denied from(const raw::tuff_plugin_capability_types_denied_t &d) {
        Denied out;
        out.tag = d.tag;
        switch (d.tag) {
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_MALFORMED:
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_NOT_REQUESTED:
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_NOT_GRANTED:
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_ADDRESS:
            out.text = String::copy(d.val.malformed);
            break;
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_UNRESOLVED:
            out.text = String::copy(d.val.unresolved);
            break;
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_RESERVED_PORT:
            out.port = d.val.reserved_port;
            break;
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_QUOTA:
            out.text = String::copy(d.val.quota);
            break;
        case TUFF_PLUGIN_CAPABILITY_TYPES_DENIED_FAILED:
            out.text = String::copy(d.val.failed);
            break;
        default:
            break;
        }
        return out;
    }
};

struct HttpResponse {
    uint16_t status = 0;
    Bytes body;
    // The URL that answered, after redirects.
    String url;

    bool ok() const { return status >= 200 && status < 300; }
};

struct FileEntry {
    String name;
    bool is_dir = false;
    uint64_t size = 0;
    uint64_t modified_ms = 0;

    static FileEntry from(const raw::tuff_plugin_capability_types_file_entry_t &e) {
        FileEntry out;
        out.name = String::copy(e.name);
        out.is_dir = e.is_dir;
        out.size = e.size;
        out.modified_ms = e.modified_ms;
        return out;
    }
};

// What a file operation gave: a read's bytes, a listing's entries, a stat's
// entry, or nothing (write, remove, rename, create_dir).
struct FileResult {
    uint8_t tag = TUFF_PLUGIN_CAPABILITY_TYPES_FILE_RESULT_DONE;
    Bytes data;
    detail::Vec<FileEntry> entries;
    FileEntry entry;
};

using HttpOutcome = Outcome<HttpResponse, Denied>;
using FileOutcome = Outcome<FileResult, Denied>;

namespace detail {

struct PendingIo {
    uint32_t handle;
    uint8_t tag;
    std::coroutine_handle<> task;
    void *slot;
    void (*fill)(void *slot, IoEvent *event);
};

inline Registry<PendingIo> pending_io;

inline bool resume_io(uint32_t handle, uint8_t tag, IoEvent *event) {
    for (size_t i = 0; i < pending_io.size(); i++) {
        if (pending_io[i].handle == handle && pending_io[i].tag == tag) {
            PendingIo p = pending_io[i];
            pending_io.swap_remove(i);
            p.fill(p.slot, event);
            p.task.resume();
            return true;
        }
    }
    return false;
}

// Resumes the coroutine an http or file event completes; false for other
// events and for completions nothing waits for.
inline bool io(IoEvent *event) {
    switch (event->tag) {
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_HTTP:
        return resume_io(event->val.http.request, event->tag, event);
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_FILE:
        return resume_io(event->val.file.operation, event->tag, event);
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_BACKGROUND:
        return resume_io(event->val.background.handle, event->tag, event);
    case TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_REPLY:
        return resume_io(event->val.reply.handle, event->tag, event);
    default:
        return false;
    }
}

} // namespace detail

namespace http {

// A request being built; co_await sends it.
class Request {
  public:
    Request(Str method, Str url) : method_(String::copy(method)), url_(String::copy(url)) {}

    Request &&body(Str data) && {
        body_ = Bytes::copy(data);
        return move(*this);
    }
    // Sets the body to already serialized JSON with its content type.
    Request &&json(Str data) && {
        return move(*this).header(raw::tuff_str("content-type"), raw::tuff_str("application/json")).body(data);
    }
    Request &&json(const char *data) && { return move(*this).json(raw::tuff_str(data)); }
    Request &&header(Str name, Str value) && {
        headers_.push(Header{String::copy(name), Bytes::copy(value)});
        return move(*this);
    }
    Request &&header(const char *name, const char *value) && {
        return move(*this).header(raw::tuff_str(name), raw::tuff_str(value));
    }
    // Shorter than the owner's limit only.
    Request &&timeout(uint32_t ms) && {
        timeout_ms_ = ms;
        return move(*this);
    }
    // Ask for gzip and inflate it, bounded by the response limit.
    Request &&decompress() && {
        decompress_ = true;
        return move(*this);
    }
    Request &&redirects(uint8_t max) && {
        max_redirects_ = max;
        return move(*this);
    }

    bool await_ready() const noexcept { return false; }
    bool await_suspend(std::coroutine_handle<> task) {
        Str method = method_.view();
        Str url = url_.view();
        Vec<raw::tuff_plugin_capability_types_header_t> raw;
        for (Header &h : headers_) {
            Str name = h.name.view();
            raw.push(raw::tuff_plugin_capability_types_header_t{name, *h.value.raw()});
        }
        raw::tuff_plugin_net_http_request_t req = {};
        req.method = method;
        req.url = url;
        req.headers = {raw.begin(), raw.size()};
        req.body = *body_.raw();
        req.timeout_ms = {timeout_ms_ != 0, timeout_ms_};
        req.decompress = decompress_;
        req.max_redirects = max_redirects_;
        raw::tuff_plugin_net_handle_t handle = 0;
        raw::tuff_plugin_net_denied_t err;
        if (!raw::tuff_plugin_net_fetch(&req, &handle, &err)) {
            outcome_ = HttpOutcome(Denied::from(err));
            raw::tuff_plugin_capability_types_denied_free(&err);
            return false;
        }
        if (!detail::pending_io.push(
                {handle, TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_HTTP, task, &outcome_, &fill})) {
            raw::tuff_plugin_net_close(handle);
            return false;
        }
        return true;
    }
    HttpOutcome await_resume() { return move(outcome_); }

  private:
    struct Header {
        String name;
        Bytes value;
    };
    template <class T> using Vec = detail::Vec<T>;

    static void fill(void *slot, IoEvent *event) {
        auto &done = event->val.http.outcome;
        HttpOutcome &out = *static_cast<HttpOutcome *>(slot);
        if (done.is_err) {
            out = HttpOutcome(Denied::from(done.val.err));
            return;
        }
        HttpResponse r;
        r.status = done.val.ok.status;
        r.body = Bytes::adopt(done.val.ok.body);
        done.val.ok.body = {nullptr, 0};
        r.url = String::copy(done.val.ok.url);
        out = HttpOutcome(move(r));
    }

    String method_;
    String url_;
    Vec<Header> headers_;
    Bytes body_;
    uint32_t timeout_ms_ = 0;
    bool decompress_ = false;
    uint8_t max_redirects_ = 0;
    HttpOutcome outcome_{Denied{}};
};

inline Request get(Str url) { return Request(raw::tuff_str("GET"), url); }
inline Request get(const char *url) { return get(raw::tuff_str(url)); }
inline Request post(Str url, Str body) { return Request(raw::tuff_str("POST"), url).body(body); }
inline Request post(const char *url, Str body) { return post(raw::tuff_str(url), body); }
inline Request post(const char *url, const char *body) {
    return post(raw::tuff_str(url), raw::tuff_str(body));
}

} // namespace http

namespace files {

namespace detail_files {

// One file operation: `start` issues it and gives the handle.
template <class Start> class Operation {
  public:
    explicit Operation(Start start) : start_(move(start)) {}
    bool await_ready() const noexcept { return false; }
    bool await_suspend(std::coroutine_handle<> task) {
        raw::tuff_plugin_files_handle_t handle = 0;
        raw::tuff_plugin_files_denied_t err;
        if (!start_(&handle, &err)) {
            outcome_ = FileOutcome(Denied::from(err));
            raw::tuff_plugin_capability_types_denied_free(&err);
            return false;
        }
        if (!detail::pending_io.push(
                {handle, TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_FILE, task, &outcome_, &fill})) {
            return false;
        }
        return true;
    }
    FileOutcome await_resume() { return move(outcome_); }

  private:
    static void fill(void *slot, IoEvent *event) {
        auto &done = event->val.file.outcome;
        FileOutcome &out = *static_cast<FileOutcome *>(slot);
        if (done.is_err) {
            out = FileOutcome(Denied::from(done.val.err));
            return;
        }
        FileResult r;
        auto &ok = done.val.ok;
        r.tag = ok.tag;
        if (ok.tag == TUFF_PLUGIN_CAPABILITY_TYPES_FILE_RESULT_DATA) {
            r.data = Bytes::adopt(ok.val.data);
            ok.val.data = {nullptr, 0};
        } else if (ok.tag == TUFF_PLUGIN_CAPABILITY_TYPES_FILE_RESULT_ENTRIES) {
            for (size_t i = 0; i < ok.val.entries.len; i++) {
                r.entries.push(FileEntry::from(ok.val.entries.ptr[i]));
            }
        } else if (ok.tag == TUFF_PLUGIN_CAPABILITY_TYPES_FILE_RESULT_ENTRY) {
            r.entry = FileEntry::from(ok.val.entry);
        }
        out = FileOutcome(move(r));
    }

    Start start_;
    FileOutcome outcome_{Denied{}};
};

template <class Start> Operation<Start> operation(Start start) {
    return Operation<Start>(move(start));
}

} // namespace detail_files

// Reads up to `len` bytes of a file from `offset` (paths are relative to
// the plugin's own store).
inline auto read(Str path, uint64_t offset = 0, uint32_t len = 0x100000) {
    return detail_files::operation(
        [=](raw::tuff_plugin_files_handle_t *h, raw::tuff_plugin_files_denied_t *e) mutable {
            return raw::tuff_plugin_files_read(&path, offset, len, h, e);
        });
}
inline auto read(const char *path, uint64_t offset = 0, uint32_t len = 0x100000) {
    return read(raw::tuff_str(path), offset, len);
}

namespace detail_files {
inline auto write_with(Str path, Str data, uint8_t mode, uint64_t at) {
    return operation(
        [=](raw::tuff_plugin_files_handle_t *h, raw::tuff_plugin_files_denied_t *e) mutable {
            raw::tuff_plugin_files_write_mode_t m;
            m.tag = mode;
            m.val.at = at;
            raw::plugin_list_u8_t bytes = {const_cast<uint8_t *>(data.ptr), data.len};
            return raw::tuff_plugin_files_write(&path, &m, &bytes, h, e);
        });
}
} // namespace detail_files

// Creates or truncates the file, then writes.
inline auto write(Str path, Str data) {
    return detail_files::write_with(path, data, TUFF_PLUGIN_CAPABILITY_TYPES_WRITE_MODE_REPLACE, 0);
}
inline auto write(const char *path, Str data) { return write(raw::tuff_str(path), data); }
inline auto write(const char *path, const char *data) {
    return write(raw::tuff_str(path), raw::tuff_str(data));
}
inline auto append(Str path, Str data) {
    return detail_files::write_with(path, data, TUFF_PLUGIN_CAPABILITY_TYPES_WRITE_MODE_APPEND, 0);
}
inline auto append(const char *path, const char *data) {
    return append(raw::tuff_str(path), raw::tuff_str(data));
}
// Writes at `offset` of an existing file.
inline auto write_at(Str path, uint64_t offset, Str data) {
    return detail_files::write_with(path, data, TUFF_PLUGIN_CAPABILITY_TYPES_WRITE_MODE_AT, offset);
}

#define TUFF_PATH_OP(name, host)                                                                   \
    inline auto name(Str path) {                                                                   \
        return detail_files::operation([=](::tuff::raw::tuff_plugin_files_handle_t *h,             \
                                           ::tuff::raw::tuff_plugin_files_denied_t *e) mutable {   \
            return host(&path, h, e);                                                              \
        });                                                                                        \
    }                                                                                              \
    inline auto name(const char *path) { return name(::tuff::raw::tuff_str(path)); }

TUFF_PATH_OP(list, raw::tuff_plugin_files_list_dir)
TUFF_PATH_OP(stat, raw::tuff_plugin_files_stat)
TUFF_PATH_OP(remove, raw::tuff_plugin_files_remove)
TUFF_PATH_OP(create_dir, raw::tuff_plugin_files_create_dir)
#undef TUFF_PATH_OP

inline auto rename(Str from, Str to) {
    return detail_files::operation(
        [=](raw::tuff_plugin_files_handle_t *h, raw::tuff_plugin_files_denied_t *e) mutable {
            return raw::tuff_plugin_files_rename(&from, &to, h, e);
        });
}
inline auto rename(const char *from, const char *to) {
    return rename(raw::tuff_str(from), raw::tuff_str(to));
}

// Bytes used and the quota, from the host's running count.
struct Usage {
    uint64_t used;
    uint64_t quota;
};
inline Usage usage() {
    raw::plugin_tuple2_u64_u64_t r;
    raw::tuff_plugin_files_usage(&r);
    return Usage{r.f0, r.f1};
}

} // namespace files

// Why a background task gave no output: `kind` is one of the constants,
// `text` the message (the refusal's text for REFUSED).
struct BackgroundError {
    static constexpr uint8_t FAILED = TUFF_PLUGIN_CAPABILITY_TYPES_BACKGROUND_ERROR_FAILED;
    static constexpr uint8_t FAULTED = TUFF_PLUGIN_CAPABILITY_TYPES_BACKGROUND_ERROR_FAULTED;
    static constexpr uint8_t TOO_LARGE = TUFF_PLUGIN_CAPABILITY_TYPES_BACKGROUND_ERROR_TOO_LARGE;
    // The host refused to start the task (bad name, input too large, four
    // tasks already in flight).
    static constexpr uint8_t REFUSED = 3;

    uint8_t kind = FAULTED;
    String text;
};

using BackgroundOutcome = Outcome<Bytes, BackgroundError>;

// A background task being started; co_await runs P::background(task, input)
// on a separate instance of the module, where every host call traps, and
// gives its output. At most four per plugin are queued or running, and
// input and output are capped by the message limit (256 KiB by default).
class Background {
  public:
    Background(Str task, Str input) : task_(task), input_(input) {}

    bool await_ready() const noexcept { return false; }
    bool await_suspend(std::coroutine_handle<> coroutine) {
        raw::plugin_list_u8_t bytes = {const_cast<uint8_t *>(input_.ptr), input_.len};
        raw::tuff_plugin_background_handle_t handle = 0;
        raw::tuff_plugin_background_denied_t err;
        if (!raw::tuff_plugin_background_run(&task_, &bytes, &handle, &err)) {
            BackgroundError refused;
            refused.kind = BackgroundError::REFUSED;
            refused.text = Denied::from(err).text;
            outcome_ = BackgroundOutcome(move(refused));
            raw::tuff_plugin_capability_types_denied_free(&err);
            return false;
        }
        if (!detail::pending_io.push({handle, TUFF_PLUGIN_CAPABILITY_TYPES_IO_EVENT_BACKGROUND,
                                      coroutine, &outcome_, &fill})) {
            raw::tuff_plugin_background_cancel(handle);
            return false;
        }
        return true;
    }
    BackgroundOutcome await_resume() { return move(outcome_); }

  private:
    static void fill(void *slot, IoEvent *event) {
        auto &done = event->val.background.outcome;
        BackgroundOutcome &out = *static_cast<BackgroundOutcome *>(slot);
        if (!done.is_err) {
            out = BackgroundOutcome(Bytes::adopt(done.val.ok));
            done.val.ok = {nullptr, 0};
            return;
        }
        BackgroundError error;
        error.kind = done.val.err.tag;
        if (error.kind == BackgroundError::FAILED) {
            error.text = String::copy(done.val.err.val.failed);
        } else if (error.kind == BackgroundError::FAULTED) {
            error.text = String::copy(done.val.err.val.faulted);
        }
        out = BackgroundOutcome(move(error));
    }

    Str task_;
    Str input_;
    BackgroundOutcome outcome_{BackgroundError{}};
};

inline Background background(Str task, Str input) { return Background(task, input); }
inline Background background(const char *task, Str input) {
    return Background(raw::tuff_str(task), input);
}

} // namespace tuff

#endif
