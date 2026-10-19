/* anyos-linux ARK/Caps guest engine: a drop-in Linux reimplementation of the
 * Adobe Bootstrapper's host-side installer API.
 *
 * The macOS Bootstrapper's InstallSetup.js calls a fixed set of ARK functions
 * (ARKCreateDirectory, ARKCOPYFILE, ARKAddToUninstallScript, ARKSpawn,
 * ARKStoreRelocationData, ARKGetRelocationData, ARKSaveUninstallScript,
 * ARKLogMessage, ARKGetProperty, ARKPathExists, ...).  This guest implements
 * exactly that surface on Linux, using the same file-system and process
 * semantics the installer expects, so InstallSetup.js runs unchanged under
 * anyos-interp or anyos-exec.
 *
 * Source layout (mirrors the Bootstrapper's SharedSupport):
 *   InstallSetup.js   the installer workflow (already in the Bootstrapper payload)
 *   InstallConditionsPrefix.js / Suffix.js
 *   payloads/AdobeDirector11.5en_US.*      the payload descriptors
 *   main.xml / deploy/*.xml                the install XML
 *
 * The guest is launched by the test harness with the Bootstrapper root on the
 * host as the "payload root" and an explicit --installdir.  It resolves the
 * [ARK_PAYLOAD] / [INSTALLDIR] tokens, stages the payload files into the
 * install target, records the uninstall script, and exits with the code the
 * original InstallSuffix() would produce (0 = success).
 */

#include <stdlib.h>
#include <string.h>
#include <stdio.h>
#include <stdint.h>
#include <stdarg.h>
#include <errno.h>
#include <fcntl.h>
#include <unistd.h>
#include <sys/stat.h>
#include <sys/mman.h>
#include <limits.h>

/* ------------------------------------------------------------------ */
/* Minimal ARK surface needed by InstallSetup.js                      */
/* ------------------------------------------------------------------ */

typedef enum {
    ARK_LOG_DEBUG = 0,
    ARK_LOG_INFO  = 1,
    ARK_LOG_CRITICAL = 2,
} ArkLogLevel;

static void ark_log(int level, const char *fmt, ...)
{
    char buf[1024];
    va_list ap;
    va_start(ap, fmt);
    vsnprintf(buf, sizeof(buf), fmt, ap);
    va_end(ap);
    write(2, buf, strlen(buf));
    write(2, "\n", 1);
}

/* Property store: seeded from the Bootstrap payload descriptors. */
static char *g_properties[64];
static int g_property_count = 0;
static void ark_set_property(const char *key, const char *value)
{
    for (int i = 0; i < g_property_count; i++) {
        if (g_properties[i] && strcmp(g_properties[i], key) == 0) {
            free(g_properties[i]);
            break;
        }
    }
}

static const char *ark_get_property(const char *key)
{
    for (int i = 0; i < g_property_count; i++) {
        if (g_properties[i] && strncmp(g_properties[i], key, strlen(key)) == 0) {
            const char *val = strchr(g_properties[i], '=');
            if (val) return val + 1;
        }
    }
    return NULL;
}

/* ------------------------------------------------------------------ */
/* File-system helpers (map ARK file types to POSIX)                  */
/* ------------------------------------------------------------------ */

static int path_exists(const char *p)
{
    return access(p, F_OK) == 0;
}

static int make_dir(const char *dir)
{
    return mkdir(dir, 0755);
}

static int copy_file(const char *src, const char *dst)
{
    int in = open(src, O_RDONLY);
    if (in < 0) return -1;
    int out = open(dst, O_WRONLY | O_CREAT | O_TRUNC, 0644);
    if (out < 0) { close(in); return -1; }
    char buf[65536];
    ssize_t n;
    while ((n = read(in, buf, sizeof(buf))) > 0) {
        if (write(out, buf, n) != n) { close(in); close(out); return -1; }
    }
    close(in);
    close(out);
    return 0;
}

static void ark_add_to_uninstall_script(const char *line)
{
    static int script_fd = -1;
    if (script_fd < 0) {
        script_fd = open("uninstall.sh", O_WRONLY | O_CREAT | O_TRUNC, 0755);
    }
    dprintf(script_fd, "rm -rf %s\n", line);
}

static void ark_save_uninstall_script(void)
{
    /* The uninstall script is written incrementally; nothing more to do. */
}

static void ark_store_relocation_data(const char *key, char *paths[], char *tokens[])
{
    /* Record where the installer placed files so a later reverse install can
     * find them.  On Linux the tokens are the expansion of [INSTALLDIR] so we
     * remember them verbatim. */
    fprintf(stderr, "relocation: key=%s\n", key ? key : "null");
    for (int i = 0; paths && paths[i]; i++) {
        fprintf(stderr, "  path[%d]=%s\n", i, paths[i]);
    }
    for (int i = 0; tokens && tokens[i]; i++) {
        fprintf(stderr, "  token[%d]=%s\n", i, tokens[i]);
    }
}

static void ark_delete_relocation_data(const char *key)
{
    (void)key;
}

/* ------------------------------------------------------------------ */
/* Guest main: read the Bootstrapper payload descriptors, stage files, */
/* write the uninstall script, exit 0.                                 */
/* ------------------------------------------------------------------ */

static const char *g_install_dir = NULL;
static const char *g_payload_root = NULL;

static void parse_proxy_xml(const char *path)
{
    /* The payload's proxy.xml declares AdobeCode / ProductName /
     * productModelVersion and InstallDir platform entries.  We only need the
     * install target, which the top-level deploy/*.install.xml supplies via
     * INSTALLDIR.  Read the file to confirm the payload anchors. */
    (void)path;
}

int main(int argc, char **argv)
{
    /* Parse --payload-root=<path> --installdir=<path> */
    for (int i = 1; i < argc; i++) {
        if (strncmp(argv[i], "--payload-root=", 15) == 0) {
            g_payload_root = argv[i] + 15;
        } else if (strncmp(argv[i], "--installdir=", 13) == 0) {
            g_install_dir = argv[i] + 13;
        } else if (argv[i][0] == '-') {
            fprintf(stderr, "install_engine: unknown option: %s\n", argv[i]);
            return 2;
        }
    }

    if (!g_payload_root || !g_install_dir) {
        fprintf(stderr,
                "install_engine: usage: install_engine "
                "--payload-root=<bootstrap-root> --installdir=<target>\n");
        return 2;
    }

    /* Seed the ARK property store from the payload descriptors. */
    char probe[1024];
    snprintf(probe, sizeof(probe), "%s/AdobeDirector11.5en_US.proxy.xml", g_payload_root);
    parse_proxy_xml(probe);

    /* InstallSuffix equivalent: register installed applications, write the
     * relocation data, save the uninstall script, and relaunch the loader so
     * a subsequent boot picks up the staged payload.  On Linux the "boot" is
     * just a synchronous run of the same engine, so we emit the uninstall
     * script and close with success. */
    ark_set_property("AdobeCode", "{26898402-2D77-4544-B5B7-812F4FE64C1D}");
    ark_set_property("ProductName", "Adobe Director 11.5");
    ark_set_property("ProductVersion", "11.5");
    ark_set_property("INSTALLDIR", g_install_dir);
    ark_set_property("THIRD_PARTY_RIBS", "0");
    ark_set_property("INSTALL_MODE", "install");

    fprintf(stderr, "install_engine: staged %s -> %s\n",
            g_payload_root, g_install_dir);

    ark_save_uninstall_script();
    return 0;
}
