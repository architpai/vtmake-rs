# Print the peak bytes of temporary files under DIR while process PID runs.
# Counts files in DIR and deleted files under DIR that any process holds open.
import os, sys, time
d, pid = os.path.realpath(sys.argv[1]), int(sys.argv[2])
peak = 0
def size():
    total, seen = 0, set()
    for root, _, files in os.walk(d):
        for f in files:
            try:
                st = os.stat(os.path.join(root, f)); seen.add(st.st_ino); total += st.st_blocks * 512
            except OSError: pass
    for p in os.listdir('/proc'):
        if not p.isdigit(): continue
        fdd = f'/proc/{p}/fd'
        try: fds = os.listdir(fdd)
        except OSError: continue
        for fd in fds:
            try:
                t = os.readlink(f'{fdd}/{fd}')
                if t.startswith(d) and t.endswith(' (deleted)'):
                    st = os.stat(f'{fdd}/{fd}')
                    if st.st_ino not in seen: seen.add(st.st_ino); total += st.st_blocks * 512
            except OSError: pass
    return total
while True:
    if not os.path.exists(f'/proc/{pid}'): break
    peak = max(peak, size()); time.sleep(0.5)
print(peak)
