
#define _GNU_SOURCE

#include <dlfcn.h>
#include <stdint.h>
#include <stdlib.h>
#include <stdio.h>
#include <stdarg.h>
#include <sys/syscall.h>
#include <sys/random.h>

/* the getrandom() crate makes the following call - we intercept it!

unsafe fn getrandom(
    buf: *mut libc::c_void,
    buflen: libc::size_t,
    flags: libc::c_uint,
) -> libc::ssize_t {
    libc::syscall(libc::SYS_getrandom, buf, buflen, flags) as libc::ssize_t
}

*/

__thread void* libc_syscall_fn = NULL;

ssize_t syscall(long call, ...) {
    if (call == SYS_getrandom) {
      va_list args;
      va_start(args, call);
      void* buf = va_arg(args, void*);
      size_t len = va_arg(args, size_t);
      uint32_t flags = va_arg(args, uint32_t);
      return getrandom(buf, len, flags);
    }

    if (libc_syscall_fn == NULL) {
      libc_syscall_fn = dlsym(RTLD_NEXT, "syscall");
    }

    // only way to forward varargs is with gcc builtins, but we only need this on
    // linux so its okay to be non-portable.
    void *args = __builtin_apply_args();
    void *ret = __builtin_apply((void (*)())libc_syscall_fn, args, 64 * 8);
    __builtin_return(ret);
}
