/* Read-only macOS mapped-image inode/start-time observation. No App launch,
 * signals, credentials, privilege changes, TCC access or process memory reads.
 * Compile from reviewed source, pin the observer binary SHA outside a package.
 */
#include <stdio.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <limits.h>

#ifdef __APPLE__
#include <errno.h>
#include <fcntl.h>
#include <libproc.h>
#include <mach/vm_prot.h>
#include <sys/proc_info.h>
#include <sys/stat.h>
#include <unistd.h>

static int reject(const char *reason) {
    printf("{\"schema\":1,\"verified\":false,\"reason\":\"%s\"}\n", reason);
    return 1;
}

int main(int argc, char **argv) {
    if (argc != 3 || argv[2][0] != '/') return reject("invalid_arguments");
    char *end = NULL;
    long number = strtol(argv[1], &end, 10);
    if (!end || *end || number <= 0 || number > INT_MAX) return reject("invalid_pid");
    int pid = (int)number;
    struct proc_bsdinfo before, after;
    memset(&before, 0, sizeof(before)); memset(&after, 0, sizeof(after));
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &before, sizeof(before)) != sizeof(before))
        return reject("native_process_query_denied");
    if (before.pbi_uid != getuid()) return reject("different_owner");
    char image[PROC_PIDPATHINFO_MAXSIZE]; memset(image, 0, sizeof(image));
    if (proc_pidpath(pid, image, sizeof(image)) <= 0) return reject("native_image_path_denied");
    if (strcmp(image, argv[2]) != 0) return reject("different_image_path");
    int fd = open(argv[2], O_RDONLY | O_NOFOLLOW);
    if (fd < 0) return reject("disk_image_unavailable");
    struct stat disk;
    if (fstat(fd, &disk) || !S_ISREG(disk.st_mode)) { close(fd); return reject("invalid_disk_image"); }
    uint64_t start = before.pbi_start_tvsec * 1000000000ULL + before.pbi_start_tvusec * 1000ULL;
    uint64_t change = (uint64_t)disk.st_ctimespec.tv_sec * 1000000000ULL + (uint64_t)disk.st_ctimespec.tv_nsec;
    uint64_t modified = (uint64_t)disk.st_mtimespec.tv_sec * 1000000000ULL + (uint64_t)disk.st_mtimespec.tv_nsec;
    if (change > start || modified > start) { close(fd); return reject("image_changed_after_start"); }
    int matched = 0;
    uint64_t address = 0;
    for (unsigned int i = 0; i < 65536; i++) {
        struct proc_regionwithpathinfo region; memset(&region, 0, sizeof(region));
        if (proc_pidinfo(pid, PROC_PIDREGIONPATHINFO, address, &region, sizeof(region)) != sizeof(region))
            break;
        if (strcmp(region.prp_vip.vip_path, argv[2]) == 0 &&
            (region.prp_prinfo.pri_protection & VM_PROT_EXECUTE)) {
            const struct vinfo_stat *mapped = &region.prp_vip.vip_vi.vi_stat;
            if (mapped->vst_ino != (uint64_t)disk.st_ino || mapped->vst_dev != (uint32_t)disk.st_dev) {
                close(fd); return reject("old_mapped_image");
            }
            matched = 1; break;
        }
        uint64_t next = region.prp_prinfo.pri_address + region.prp_prinfo.pri_size;
        if (next <= address) break;
        address = next;
    }
    if (!matched) { close(fd); return reject("mapped_image_unavailable"); }
    if (proc_pidinfo(pid, PROC_PIDTBSDINFO, 0, &after, sizeof(after)) != sizeof(after) ||
        after.pbi_start_tvsec != before.pbi_start_tvsec || after.pbi_start_tvusec != before.pbi_start_tvusec ||
        after.pbi_pid != before.pbi_pid) { close(fd); return reject("process_changed"); }
    struct stat final;
    if (fstat(fd, &final) || final.st_ino != disk.st_ino || final.st_dev != disk.st_dev ||
        final.st_size != disk.st_size || final.st_ctimespec.tv_sec != disk.st_ctimespec.tv_sec ||
        final.st_ctimespec.tv_nsec != disk.st_ctimespec.tv_nsec) { close(fd); return reject("image_changed"); }
    close(fd);
    printf("{\"schema\":1,\"verified\":true,\"pid\":%d,\"start_unix_ns\":%llu,"
           "\"device\":%llu,\"inode\":%llu,\"change_unix_ns\":%llu}\n",
           pid, (unsigned long long)start, (unsigned long long)disk.st_dev,
           (unsigned long long)disk.st_ino, (unsigned long long)change);
    return 0;
}
#else
int main(void) {
    puts("{\"schema\":1,\"verified\":false,\"reason\":\"native_platform_unsupported\"}");
    return 1;
}
#endif
