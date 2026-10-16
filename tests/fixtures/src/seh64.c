/* x64 structured exception handling, the way a 64-bit Windows program does it.
 *
 * There is no chain of frames in FS:[0] here. Every function with a `try` block
 * gets a RUNTIME_FUNCTION entry in .pdata pointing at UNWIND_INFO, which names a
 * language handler and carries the C scope table of the function's `__try`
 * statements. Raising an exception walks the function table up the stack calling
 * those handlers; the one that takes the exception calls RtlUnwindEx, which runs
 * the `finally` bodies of the frames in between (running each language handler
 * again with EXCEPTION_UNWINDING set) and then continues inside the `except`
 * block, on the target frame's own stack.
 *
 * clang emits references to `__C_specific_handler`, which normally comes from
 * vcruntime140.dll. These fixtures have no CRT at all, so the fixture provides
 * that handler itself, the way the CRT does: it finds the scope covering the
 * control PC, runs `__except` filters while searching, and runs `__finally`
 * bodies while unwinding. It is compiled by clang, so its scope tables, filters
 * and function table entries are exactly what a real 64-bit build produces.
 *
 * Only built for x64: the 32-bit FS:[0] chain is covered by apitest.c. Each
 * failed check prints "FAIL <name>"; the program prints "ok" and exits with 0
 * when everything passes, otherwise it exits with the number of failures. */

#include "win.h"

/* ---- the pieces of the x64 ABI this fixture is written against ---------- */

typedef struct _EXCEPTION_RECORD {
    DWORD ExceptionCode;
    DWORD ExceptionFlags;
    struct _EXCEPTION_RECORD *ExceptionRecord;
    void *ExceptionAddress;
    DWORD NumberParameters;
    DWORD Reserved;
    ULONG_PTR ExceptionInformation[15];
} EXCEPTION_RECORD;

typedef struct {
    EXCEPTION_RECORD *ExceptionRecord;
    void *ContextRecord;
} EXCEPTION_POINTERS;

/* What the runtime tells a language handler about the frame it is called for. */
typedef struct {
    void *ControlPc;
    void *ImageBase;
    void *FunctionEntry;
    void *EstablisherFrame;
    void *TargetIp;
    void *ContextRecord;
    void *LanguageHandler;
    void *HandlerData;
    void *HistoryTable;
    DWORD ScopeIndex;
    DWORD Reserved;
} DISPATCHER_CONTEXT;

/* A C scope table: image-relative addresses of the `try` body, of the filter or
 * the `finally` body, and of the landing pad after an `__except`. A `__finally`
 * has no landing pad, so its Jump is zero; a catch-all `__except(1)` has the
 * literal 1 as its handler. */
typedef struct {
    DWORD Begin;
    DWORD End;
    DWORD Handler;
    DWORD Jump;
} SCOPE_RECORD;

typedef struct {
    DWORD Count;
    SCOPE_RECORD Record[1];
} SCOPE_TABLE;

#define EXCEPTION_UNWINDING 0x02
#define EXCEPTION_CONTINUE_EXECUTION (-1)
#define EXCEPTION_CONTINUE_SEARCH 0
#define EXCEPTION_EXECUTE_HANDLER 1

/* The exception code Delphi and Inno Setup use for software exceptions. */
#define SEH_TEST_CODE 0x0EEDFADE

/* ---- the CRT's __C_specific_handler, which there is no CRT to provide ---- */

/* Run the `finally` body of a scope. clang emits it as a funclet that either
 * takes no arguments or reads its parent frame from RDX, so both are passed. */
static void run_cleanup(DWORD handler, ULONG_PTR base, void *frame)
{
    void (*cleanup)(void *, void *);
    cleanup = (void (*)(void *, void *))(base + handler);
    cleanup(frame, frame);
}

/* Whether the fixture's handler unwinds with RtlUnwind (the four-argument form,
 * which has no context of its own) instead of RtlUnwindEx. */
static volatile int use_rtl_unwind;

int __stdcall __C_specific_handler(EXCEPTION_RECORD *record, void *frame,
                                   void *context, DISPATCHER_CONTEXT *dispatcher)
{
    SCOPE_TABLE *table = (SCOPE_TABLE *)dispatcher->HandlerData;
    ULONG_PTR base = (ULONG_PTR)dispatcher->ImageBase;
    ULONG_PTR ip = (ULONG_PTR)dispatcher->ControlPc - base;
    int unwinding = (record->ExceptionFlags & EXCEPTION_UNWINDING) != 0;

    /* The table is sorted by begin address, and the records that cover one piece
     * of code are ordered innermost first, so scanning it forwards asks the
     * scopes covering the control PC from the inside out -- the order Windows
     * asks them in. A scope that passes the exception on is left for the next
     * one out. */
    for (DWORD i = 0; i < table->Count; i++) {
        SCOPE_RECORD *scope = &table->Record[i];
        if (ip < scope->Begin) break;
        if (ip >= scope->End) continue;

        if (unwinding) {
            if (scope->Jump == 0) {
                /* A local unwind whose target is still inside this scope has not
                 * left it, so this `__finally` has yet to run. */
                ULONG_PTR target = (ULONG_PTR)dispatcher->TargetIp - base;
                if (dispatcher->TargetIp && target > scope->Begin && target < scope->End) {
                    continue;
                }
                run_cleanup(scope->Handler, base, frame);
            }
            continue;
        }

        /* While searching, a `__finally` has nothing to say. */
        if (scope->Jump == 0) continue;

        int verdict = EXCEPTION_EXECUTE_HANDLER; /* `__except(1)`: no filter */
        if (scope->Handler != (DWORD)EXCEPTION_EXECUTE_HANDLER) {
            EXCEPTION_POINTERS pointers;
            int (*filter)(void *, void *);
            pointers.ExceptionRecord = record;
            pointers.ContextRecord = context;
            filter = (int (*)(void *, void *))(base + scope->Handler);
            verdict = filter(&pointers, frame);
        }
        if (verdict == EXCEPTION_CONTINUE_EXECUTION) return 0;
        if (verdict == EXCEPTION_CONTINUE_SEARCH) continue;

        /* Take the exception: unwind to this frame and continue in the handler.
         * The return value is what the `except` body finds in Rax. */
        if (use_rtl_unwind) {
            RtlUnwind(frame, (void *)(base + scope->Jump), record,
                      (void *)(ULONG_PTR)record->ExceptionCode);
        } else {
            RtlUnwindEx(frame, (void *)(base + scope->Jump), record,
                        (void *)(ULONG_PTR)record->ExceptionCode, context,
                        dispatcher->HistoryTable);
        }
        return 1; /* not reached */
    }
    return 1; /* ExceptionContinueSearch */
}

/* ---- fixture plumbing --------------------------------------------------- */

static HANDLE out;
static int failures;

void *memset(void *d, int c, SIZE_T n) {
    unsigned char *p = d;
    while (n--) *p++ = (unsigned char)c;
    return d;
}

static void say(const char *s) {
    DWORD n;
    WriteFile(out, s, lstrlenA(s), &n, 0);
}

static void check(int ok, const char *name) {
    if (ok) return;
    failures++;
    say("FAIL ");
    say(name);
    say("\n");
}

/* ---- the tests ---------------------------------------------------------- */

/* Raise from a function of its own, so the exception leaves a frame with no
 * handler of its own and the walk has to unwind one. */
static void raise_deep(DWORD code)
{
    RaiseException(code, 0, 0, 0);
}

/* A `__try/__except` around a call: the filter sees the exception record, and
 * the handler runs on the frame of the function that caught it. */
static volatile int filter_calls;
static volatile DWORD filter_code;
static volatile void *filter_address;
static volatile int handler_ran;
static volatile int after_handler;
static volatile int marker_read;

static int __stdcall filter_takes(EXCEPTION_POINTERS *pointers)
{
    filter_calls++;
    filter_code = pointers->ExceptionRecord->ExceptionCode;
    filter_address = pointers->ExceptionRecord->ExceptionAddress;
    return EXCEPTION_EXECUTE_HANDLER;
}

static void except_around_a_call(int through_rtl_unwind)
{
    volatile int marker = 0x5A5A;

    filter_calls = 0;
    use_rtl_unwind = through_rtl_unwind;

    __try {
        raise_deep(SEH_TEST_CODE);
        marker = 0; /* not reached */
    } __except (filter_takes(_exception_info())) {
        /* Reached through the unwind, on this function's own frame: a local from
         * before the `try` still has to be readable here. */
        marker_read = marker;
        handler_ran = 1;
    }
    after_handler = 1;

    use_rtl_unwind = 0;

    check(filter_calls == 1, "x64 __except filter runs once");
    check(filter_code == SEH_TEST_CODE, "x64 filter sees the exception code");
    check(filter_address != 0, "x64 filter sees the exception address");
    check(handler_ran == 1, "x64 __except body runs");
    check(marker_read == 0x5A5A, "x64 handler runs on the frame it caught in");
    check(after_handler == 1, "x64 execution continues after the handler");
}

/* A filter that passes the exception on leaves it for the next scope: the outer
 * `__except` has to catch it instead. */
static volatile int filter_saw_record;
static volatile int inner_ran;
static volatile int outer_ran;

static int __stdcall filter_searches(EXCEPTION_POINTERS *pointers)
{
    filter_saw_record = pointers != 0 && pointers->ExceptionRecord != 0;
    return EXCEPTION_CONTINUE_SEARCH;
}

static void filter_can_pass_it_on(void)
{
    inner_ran = outer_ran = 0;

    __try {
        __try {
            raise_deep(SEH_TEST_CODE);
        } __except (filter_searches(_exception_info())) {
            inner_ran = 1;
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        outer_ran = 1;
    }

    check(filter_saw_record, "x64 filter is given the exception pointers");
    check(inner_ran == 0, "x64 filter can pass the exception on");
    check(outer_ran == 1, "the outer x64 handler then catches it");
}

/* A `__finally` in a frame between the raise and the handler runs while the
 * exception is on its way out, before the `except` body it unwinds to. */
static volatile int finally_ran;
static volatile int finally_saw_except;
static volatile int except_stage;

static void finally_then_except(void)
{
    finally_ran = finally_saw_except = except_stage = 0;

    __try {
        __try {
            raise_deep(SEH_TEST_CODE);
        } __finally {
            finally_ran = 1;
            finally_saw_except = except_stage;
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        except_stage = 1;
    }

    check(finally_ran == 1, "x64 __finally runs while unwinding");
    check(finally_saw_except == 0, "x64 __finally runs before the __except body");
    check(except_stage == 1, "x64 __except runs after the __finally");
}

/* Three frames on the way out: both `finally` bodies run, the inner one first. */
static volatile int outer_finally;
static volatile int inner_finally_was_first;

static void nested_finallys(void)
{
    outer_finally = inner_finally_was_first = except_stage = 0;

    __try {
        __try {
            __try {
                raise_deep(SEH_TEST_CODE);
            } __finally {
                inner_finally_was_first = outer_finally == 0;
            }
        } __finally {
            outer_finally = 1;
        }
    } __except (EXCEPTION_EXECUTE_HANDLER) {
        except_stage = 2;
    }

    check(inner_finally_was_first == 1, "the inner x64 __finally runs first");
    check(outer_finally == 1, "the outer x64 __finally runs too");
    check(except_stage == 2, "the x64 __except runs last");
}

/* The ordinary path: a `__finally` runs when its `try` block finishes normally,
 * which the compiler arranges itself. */
static volatile int normal_finally;
static volatile int normal_body;

static void finally_without_exception(void)
{
    normal_finally = normal_body = 0;

    __try {
        normal_body = 1;
    } __finally {
        normal_finally = 1;
    }

    check(normal_body == 1 && normal_finally == 1, "x64 __finally runs on the normal path");
}

int mainCRTStartup(void)
{
    out = GetStdHandle(STD_OUTPUT_HANDLE);

    except_around_a_call(0);
    except_around_a_call(1);
    filter_can_pass_it_on();
    finally_then_except();
    nested_finallys();
    finally_without_exception();

    if (failures == 0) say("ok\n");
    ExitProcess((UINT)failures);
    return 0;
}
