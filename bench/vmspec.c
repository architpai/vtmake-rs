// LD_PRELOAD shim: report a small machine size to sysconf() inside the cgroup.
// A cgroup limits CPU and memory, but sysconf() still reports all the CPUs and
// memory of the host. tippecanoe sizes its sorts and spills from these values.
// BENCH_NPROC sets the CPU count and BENCH_PHYS_BYTES sets the memory size.
// Build: cc -shared -fPIC -O2 -o vmspec.so bench/vmspec.c -ldl
#define _GNU_SOURCE
#include <dlfcn.h>
#include <stdlib.h>
#include <unistd.h>

long sysconf(int name) {
	static long (*real)(int) = 0;
	if (!real) real = (long (*)(int)) dlsym(RTLD_NEXT, "sysconf");
	const char *v;
	if (name == _SC_PHYS_PAGES && (v = getenv("BENCH_PHYS_BYTES")))
		return atoll(v) / real(_SC_PAGESIZE);
	if ((name == _SC_NPROCESSORS_ONLN || name == _SC_NPROCESSORS_CONF) && (v = getenv("BENCH_NPROC")))
		return atol(v);
	return real(name);
}
