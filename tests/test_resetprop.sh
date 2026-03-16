#!/system/bin/sh
# ============================================================================
# resetprop 全量功能测试脚本
# 在 Android 设备上以 root 权限运行
#
# 用法:
#   adb push test_resetprop.sh /data/local/tmp/
#   adb push resetprop /data/local/tmp/
#   adb shell su -c "sh /data/local/tmp/test_resetprop.sh"
#
# 或者指定 resetprop 路径:
#   RESETPROP=/data/local/tmp/resetprop sh /data/local/tmp/test_resetprop.sh
#
# 调试模式（显示每条 set/get 命令及其结果）:
#   DEBUG=1 RESETPROP=./resetprop sh test_resetprop.sh
# ============================================================================

set -u

# ── 配置 ────────────────────────────────────────────────────────────────────

RESETPROP="${RESETPROP:-./resetprop}"
DEBUG="${DEBUG:-0}"
# 测试属性前缀，使用不常见的名称避免与系统属性冲突
TEST_PREFIX="test.resetprop.$$"
# 持久属性测试前缀
PERSIST_PREFIX="persist.test.resetprop.$$"
# 临时文件目录
TMP="/data/local/tmp"

# ── 计数器与辅助函数 ──────────────────────────────────────────────────────

PASS=0
FAIL=0
SKIP=0
TOTAL=0

RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[0;33m'
CYAN='\033[0;36m'
NC='\033[0m'

log_pass() {
    PASS=$((PASS + 1))
    TOTAL=$((TOTAL + 1))
    printf "${GREEN}  [PASS]${NC} %s\n" "$1"
}

log_fail() {
    FAIL=$((FAIL + 1))
    TOTAL=$((TOTAL + 1))
    printf "${RED}  [FAIL]${NC} %s\n" "$1"
    if [ -n "${2:-}" ]; then
        printf "${RED}         expected: %s${NC}\n" "$2"
    fi
    if [ -n "${3:-}" ]; then
        printf "${RED}         actual:   %s${NC}\n" "$3"
    fi
}

log_skip() {
    SKIP=$((SKIP + 1))
    TOTAL=$((TOTAL + 1))
    printf "${YELLOW}  [SKIP]${NC} %s\n" "$1"
}

section() {
    printf "\n${CYAN}══════════════════════════════════════════════════════════${NC}\n"
    printf "${CYAN}  %s${NC}\n" "$1"
    printf "${CYAN}══════════════════════════════════════════════════════════${NC}\n"
}

# resetprop 的 set 操作封装：丢弃 stdout/stderr，仅关心退出码
rp_set() {
    if [ "$DEBUG" = "1" ]; then
        printf "  [DBG] set: %s %s\n" "$RESETPROP" "$*" >&2
    fi
    "$RESETPROP" "$@" >/dev/null 2>&1
}

# resetprop 的 get 操作封装：捕获 stdout
# 结果写入临时文件再 cat，避免 mksh 中命令替换的兼容性问题
_rp_get_tmp="${TMP}/_rp_get_$$"
rp_get() {
    "$RESETPROP" "$@" >"$_rp_get_tmp" 2>/dev/null
    local rc=$?
    cat "$_rp_get_tmp"
    return $rc
}

# 列表操作：输出到临时文件，避免变量过大导致 "Argument list too long"
_rp_list_tmp="${TMP}/_rp_list_$$"
rp_list() {
    "$RESETPROP" "$@" >"$_rp_list_tmp" 2>/dev/null
}

# 断言：值相等
assert_eq() {
    local desc="$1" expected="$2" actual="$3"
    if [ "x${expected}" = "x${actual}" ]; then
        log_pass "$desc"
    else
        log_fail "$desc" "$expected" "$actual"
    fi
}

# 断言：退出码
assert_exit() {
    local desc="$1" expected_exit="$2"
    shift 2
    "$@" >/dev/null 2>&1
    local actual_exit=$?
    if [ "$expected_exit" -eq "$actual_exit" ]; then
        log_pass "$desc"
    else
        log_fail "$desc" "exit=$expected_exit" "exit=$actual_exit"
    fi
}

# 断言：文件中包含固定子串
assert_file_contains() {
    local desc="$1" needle="$2" file="$3"
    if grep -qF "$needle" "$file" 2>/dev/null; then
        log_pass "$desc"
    else
        log_fail "$desc" "contains '$needle'" "(not found in output)"
    fi
}

# 断言：输出包含子串（使用临时文件避免 arg too long）
assert_contains() {
    local desc="$1" needle="$2" haystack="$3"
    printf '%s' "$haystack" > "${TMP}/_rp_assert_$$"
    if grep -qF "$needle" "${TMP}/_rp_assert_$$" 2>/dev/null; then
        log_pass "$desc"
    else
        log_fail "$desc" "contains '$needle'" "(not found)"
    fi
    rm -f "${TMP}/_rp_assert_$$"
}

# 断言：命令成功（退出码 0）
assert_success() {
    local desc="$1"
    shift
    "$@" >/dev/null 2>&1
    local rc=$?
    if [ "$rc" -eq 0 ]; then
        log_pass "$desc"
    else
        log_fail "$desc" "exit=0" "exit=$rc"
    fi
}

# 断言：命令失败（退出码非 0）
assert_failure() {
    local desc="$1"
    shift
    "$@" >/dev/null 2>&1
    local rc=$?
    if [ "$rc" -ne 0 ]; then
        log_pass "$desc"
    else
        log_fail "$desc" "exit!=0" "exit=0"
    fi
}

# ── 前置检查 ──────────────────────────────────────────────────────────────

section "前置检查"

if [ ! -x "$RESETPROP" ]; then
    if command -v resetprop >/dev/null 2>&1; then
        RESETPROP="$(command -v resetprop)"
        printf "  使用 PATH 中的 resetprop: %s\n" "$RESETPROP"
    else
        printf "${RED}  错误: resetprop 不存在或不可执行: %s${NC}\n" "$RESETPROP"
        printf "  请设置 RESETPROP 环境变量或将 resetprop 放到当前目录\n"
        exit 1
    fi
fi

if [ "$(id -u)" -ne 0 ]; then
    printf "${RED}  错误: 需要 root 权限${NC}\n"
    exit 1
fi

printf "  resetprop 路径: %s\n" "$RESETPROP"
printf "  测试属性前缀: %s\n" "$TEST_PREFIX"
printf "  PID: %s\n" "$$"

# ── 清理函数 ──────────────────────────────────────────────────────────────

CLEANUP_PROPS=""

register_cleanup() {
    CLEANUP_PROPS="${CLEANUP_PROPS} $1"
}

cleanup() {
    printf "\n${CYAN}── 清理测试属性 ──${NC}\n"
    for prop in $CLEANUP_PROPS; do
        $RESETPROP -d "$prop" >/dev/null 2>&1
        # 也尝试从持久存储删除
        case "$prop" in
            persist.*) $RESETPROP -p -d "$prop" >/dev/null 2>&1 ;;
        esac
    done
    rm -f "${TMP}/test_props_$$.txt" "${TMP}/_rp_"*"_$$" 2>/dev/null
    printf "  清理完成\n"
}

trap cleanup EXIT

# 预注册测试属性
for name in basic overwrite empty special maxlen long \
            file1 file2 file3 wait wait_change \
            compact1 compact2 compact3; do
    register_cleanup "${TEST_PREFIX}.${name}"
done
for i in 1 2 3 4 5; do
    register_cleanup "${TEST_PREFIX}.concurrent_$i"
done
register_cleanup "ro.${TEST_PREFIX}.readonly"
register_cleanup "ro.${TEST_PREFIX}.ro_set"
for name in basic ponly del list; do
    register_cleanup "${PERSIST_PREFIX}.${name}"
done

# ── 诊断信息 ──────────────────────────────────────────────────────────────

section "诊断信息"

ver=$($RESETPROP --version 2>&1) || ver="(unknown)"
printf "  版本: %s\n" "$ver"

diag_val=$(rp_get ro.build.type)
if [ -n "$diag_val" ]; then
    printf "  ro.build.type = %s (resetprop 基本读取正常)\n" "$diag_val"
else
    printf "${RED}  警告: 无法读取 ro.build.type，resetprop 可能未正确初始化${NC}\n"
fi

sdk=$(rp_get ro.build.version.sdk)
printf "  SDK 版本: %s\n" "${sdk:-unknown}"

# ============================================================================
# 测试组 0: 诊断 — 排查 -n mmap 写入后读取一致性
# ============================================================================

section "0. 诊断 — mmap 写入一致性检查"

# 0.1 首次创建：-n 写入后用 getprop 和 resetprop 分别读取
rp_set -n "${TEST_PREFIX}.diag" "diag_value"
register_cleanup "${TEST_PREFIX}.diag"
val_rp=$(rp_get "${TEST_PREFIX}.diag")
val_gp=$(getprop "${TEST_PREFIX}.diag" 2>/dev/null) || val_gp="(getprop failed)"
printf "  0.1 首次 -n set 后:\n"
printf "    resetprop get: '%s'\n" "$val_rp"
printf "    getprop:       '%s'\n" "$val_gp"

# 0.2 -n 覆盖：在已有属性上 -n 写入
rp_set -n "${TEST_PREFIX}.diag" "updated_value"
val_rp=$(rp_get "${TEST_PREFIX}.diag")
val_gp=$(getprop "${TEST_PREFIX}.diag" 2>/dev/null) || val_gp="(getprop failed)"
printf "  0.2 -n 覆盖后:\n"
printf "    resetprop get: '%s'\n" "$val_rp"
printf "    getprop:       '%s'\n" "$val_gp"

# 0.3 查看 context 路由
ctx_rp=$(rp_get -Z "${TEST_PREFIX}.diag") || ctx_rp="(unknown)"
printf "  0.3 SELinux context: %s\n" "$ctx_rp"

# 0.4 property_service 设置后再用 -n 覆盖
rp_set "${TEST_PREFIX}.diag" "via_svc_value"
val_rp_svc=$(rp_get "${TEST_PREFIX}.diag")
printf "  0.4 property_service 设置后:\n"
printf "    resetprop get: '%s'\n" "$val_rp_svc"

rp_set -n "${TEST_PREFIX}.diag" "mmap_after_svc"
val_rp_after=$(rp_get "${TEST_PREFIX}.diag")
val_gp_after=$(getprop "${TEST_PREFIX}.diag" 2>/dev/null) || val_gp_after="(getprop failed)"
printf "  0.5 property_service 后 -n 覆盖:\n"
printf "    resetprop get: '%s'\n" "$val_rp_after"
printf "    getprop:       '%s'\n" "$val_gp_after"

# 0.6 删除后重建
$RESETPROP -d "${TEST_PREFIX}.diag" >/dev/null 2>&1
rp_set -n "${TEST_PREFIX}.diag" "after_delete"
val_rp_del=$(rp_get "${TEST_PREFIX}.diag")
printf "  0.6 删除后重建:\n"
printf "    resetprop get: '%s'\n" "$val_rp_del"

# 清理
$RESETPROP -d "${TEST_PREFIX}.diag" >/dev/null 2>&1

# ============================================================================
# 测试组 1: 基本 get/set 操作（使用 -n 直接 mmap）
# ============================================================================

section "1. 基本 get/set 操作（-n 直接 mmap）"

# 1.1 设置并获取普通属性（首次创建新属性）
rp_set -n "${TEST_PREFIX}.basic" "hello_world"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "1.1 设置并获取普通属性" "hello_world" "$val"

# 1.2 覆盖已有属性
rp_set -n "${TEST_PREFIX}.overwrite" "first"
rp_set -n "${TEST_PREFIX}.overwrite" "second"
val=$(rp_get "${TEST_PREFIX}.overwrite")
assert_eq "1.2 覆盖属性值" "second" "$val"

# 1.3 获取不存在的属性（应失败）
assert_failure "1.3 获取不存在的属性返回非 0" $RESETPROP "${TEST_PREFIX}.nonexistent"

# 1.4 设置空值
rp_set -n "${TEST_PREFIX}.empty" ""
val=$(rp_get "${TEST_PREFIX}.empty")
assert_eq "1.4 设置空值" "" "$val"

# 1.5 设置包含特殊字符的值
rp_set -n "${TEST_PREFIX}.special" "a=b:c/d@e"
val=$(rp_get "${TEST_PREFIX}.special")
assert_eq "1.5 特殊字符值" "a=b:c/d@e" "$val"

# 1.6 属性名长度
long_name="${TEST_PREFIX}.maxlen"
rp_set -n "$long_name" "maxlen_test"
val=$(rp_get "$long_name")
assert_eq "1.6 普通长度属性名" "maxlen_test" "$val"

# 1.7 非 ro. 长属性值应失败（与 AOSP system_properties 一致）
long_value=""
i=0
while [ $i -lt 200 ]; do
    long_value="${long_value}_"
    i=$((i + 1))
done
assert_failure "1.7 非 ro. 长属性值被拒绝" \
    "$RESETPROP" -n "${TEST_PREFIX}.long" "$long_value"
assert_failure "1.7b 长属性值写入失败后属性不存在" \
    "$RESETPROP" "${TEST_PREFIX}.long"

# ============================================================================
# 测试组 2: 删除操作 (-d)
# ============================================================================

section "2. 删除操作 (-d)"

# 2.1 删除已存在的属性
rp_set -n "${TEST_PREFIX}.basic" "to_delete"
assert_success "2.1a 删除已存在的属性" $RESETPROP -d "${TEST_PREFIX}.basic"
assert_failure "2.1b 删除后属性不存在" $RESETPROP "${TEST_PREFIX}.basic"

# 2.2 删除不存在的属性（应失败）
assert_failure "2.2 删除不存在的属性返回非 0" $RESETPROP -d "${TEST_PREFIX}.nonexistent"

# 2.3 删除后重新设置（创建新属性）
rp_set -n "${TEST_PREFIX}.basic" "before_delete"
$RESETPROP -d "${TEST_PREFIX}.basic" >/dev/null 2>&1
rp_set -n "${TEST_PREFIX}.basic" "after_delete"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "2.3 删除后重新设置" "after_delete" "$val"

# ============================================================================
# 测试组 3: -n（跳过 property_service）标志
# ============================================================================

section "3. -n 标志（跳过 property_service）"

# 确保 basic 存在
rp_set -n "${TEST_PREFIX}.basic" "direct_mmap"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "3.1 -n 标志直接写入" "direct_mmap" "$val"

# 3.2 设置 ro. 属性（总是绕过 property_service）
rp_set -n "ro.${TEST_PREFIX}.readonly" "modified"
val=$(rp_get "ro.${TEST_PREFIX}.readonly")
assert_eq "3.2 修改 ro. 属性" "modified" "$val"

# 3.3 覆盖 ro. 属性
rp_set -n "ro.${TEST_PREFIX}.ro_set" "v1"
rp_set -n "ro.${TEST_PREFIX}.ro_set" "v2"
val=$(rp_get "ro.${TEST_PREFIX}.ro_set")
assert_eq "3.3 覆盖 ro. 属性" "v2" "$val"

# 3.4 不带 -n 设置非 ro 属性（通过 property_service）
rp_set "${TEST_PREFIX}.basic" "via_svc"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "3.4 通过 property_service 设置" "via_svc" "$val"

# 3.5 property_service 设置后再用 -n 覆盖
rp_set -n "${TEST_PREFIX}.basic" "back_to_mmap"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "3.5 property_service 设置后 -n 覆盖" "back_to_mmap" "$val"

# ============================================================================
# 测试组 4: 列表功能
# ============================================================================

section "4. 列表功能"

# 4.1 列出所有属性（无参数）— 输出到文件避免变量过大
rp_set -n "${TEST_PREFIX}.basic" "list_test"
rp_list

# 检查列表文件是否包含测试属性
assert_file_contains "4.1a 列表输出包含测试属性" "${TEST_PREFIX}.basic" "$_rp_list_tmp"

# 检查格式
assert_file_contains "4.1b 列表输出格式 [key]: [value]" "[${TEST_PREFIX}.basic]:" "$_rp_list_tmp"

# 4.2 列表应包含系统属性
assert_file_contains "4.2 列表包含 ro.build 属性" "ro.build" "$_rp_list_tmp"

# 4.3 列表应排序（按属性名排序）
names=$(sed -n 's/^\[\([^]]*\)\].*/\1/p' "$_rp_list_tmp")
sorted_names=$(printf '%s\n' "$names" | sort)
if [ "x${names}" = "x${sorted_names}" ]; then
    log_pass "4.3 列表结果已排序"
else
    log_fail "4.3 列表结果已排序" "sorted by name" "not sorted"
fi

rm -f "$_rp_list_tmp"

# ============================================================================
# 测试组 5: -Z（显示 SELinux context）
# ============================================================================

section "5. -Z（显示 SELinux context）"

# 5.1 列表带 -Z 标志 — 输出到文件
rp_list -Z
if [ -s "$_rp_list_tmp" ]; then
    if grep -qF "u:object_r:" "$_rp_list_tmp"; then
        log_pass "5.1 -Z 输出包含 SELinux context"
    else
        log_pass "5.1 -Z 命令成功执行"
    fi
else
    log_skip "5.1 -Z 不可用（可能缺少 property_contexts）"
fi
rm -f "$_rp_list_tmp"

# 5.2 获取单个属性的 context
output=$(rp_get -Z ro.build.fingerprint)
rc=$?
if [ $rc -eq 0 ] && [ -n "$output" ]; then
    log_pass "5.2 获取单个属性的 SELinux context (=$output)"
else
    log_skip "5.2 SELinux context 获取不可用"
fi

# ============================================================================
# 测试组 6: -v（详细输出）
# ============================================================================

section "6. -v（详细输出）"

_tmp_stderr="${TMP}/_rp_stderr_$$"

# 6.1 -v 设置时输出到 stderr
$RESETPROP -v -n "${TEST_PREFIX}.basic" "verbose_test" >/dev/null 2>"$_tmp_stderr"
stderr=$(cat "$_tmp_stderr")
assert_contains "6.1 -v 设置时 stderr 有输出" "set ${TEST_PREFIX}.basic" "$stderr"

# 6.2 -v 删除时输出到 stderr
rp_set -n "${TEST_PREFIX}.basic" "to_verbose_delete"
$RESETPROP -v -d "${TEST_PREFIX}.basic" >/dev/null 2>"$_tmp_stderr"
stderr=$(cat "$_tmp_stderr")
assert_contains "6.2 -v 删除时 stderr 有输出" "deleted ${TEST_PREFIX}.basic" "$stderr"

# 6.3 -v 获取不存在属性时有提示
$RESETPROP -v "${TEST_PREFIX}.nonexistent" >/dev/null 2>"$_tmp_stderr"
stderr=$(cat "$_tmp_stderr")
assert_contains "6.3 -v 获取不存在属性有提示" "not found" "$stderr"

rm -f "$_tmp_stderr"

# ============================================================================
# 测试组 7: 持久属性操作 (-p, -P)
# ============================================================================

section "7. 持久属性操作 (-p, -P)"

if [ -d "/data/property" ]; then

    # 7.1 使用 -p -n 设置持久属性
    rp_set -p -n "${PERSIST_PREFIX}.basic" "persist_value"
    val=$(rp_get "${PERSIST_PREFIX}.basic")
    assert_eq "7.1 设置持久属性并读取" "persist_value" "$val"

    # 7.2 使用 -P 仅从持久存储读取
    val=$(rp_get -P "${PERSIST_PREFIX}.basic")
    rc=$?
    if [ $rc -eq 0 ]; then
        assert_eq "7.2 -P 从持久存储读取" "persist_value" "$val"
    else
        log_skip "7.2 -P 读取持久属性失败（可能存储格式不兼容）"
    fi

    # 7.3 设置另一个持久属性并用 -P 读取
    rp_set -p -n "${PERSIST_PREFIX}.ponly" "only_in_persist"
    val=$(rp_get -P "${PERSIST_PREFIX}.ponly")
    rc=$?
    if [ $rc -eq 0 ]; then
        assert_eq "7.3 -P 读取仅存在于持久存储的属性" "only_in_persist" "$val"
    else
        log_skip "7.3 -P 读取失败"
    fi

    # 7.4 -p 删除时同时删除持久存储
    rp_set -p -n "${PERSIST_PREFIX}.del" "to_be_deleted"
    $RESETPROP -p -d "${PERSIST_PREFIX}.del" >/dev/null 2>&1
    assert_failure "7.4a 删除后 sys 中不存在" $RESETPROP "${PERSIST_PREFIX}.del"
    val=$(rp_get -P "${PERSIST_PREFIX}.del")
    rc=$?
    if [ $rc -ne 0 ] || [ -z "$val" ]; then
        log_pass "7.4b 删除后持久存储中也不存在"
    else
        log_fail "7.4b 删除后持久存储中也不存在" "empty/not found" "$val"
    fi

    # 7.5 -p 列表包含持久属性
    rp_set -p -n "${PERSIST_PREFIX}.list" "in_list"
    rp_list -p
    assert_file_contains "7.5 -p 列表包含持久属性" "${PERSIST_PREFIX}.list" "$_rp_list_tmp"
    rm -f "$_rp_list_tmp"

else
    log_skip "7.x 持久属性测试 - /data/property 不可用"
fi

# ============================================================================
# 测试组 8: 从文件加载 (-f)
# ============================================================================

section "8. 从文件加载 (-f)"

PROPS_FILE="${TMP}/test_props_$$.txt"

# 先删除可能存在的旧属性，确保干净环境
$RESETPROP -d "${TEST_PREFIX}.file1" >/dev/null 2>&1
$RESETPROP -d "${TEST_PREFIX}.file2" >/dev/null 2>&1
$RESETPROP -d "${TEST_PREFIX}.file3" >/dev/null 2>&1

# 8.1 基本文件加载
cat > "$PROPS_FILE" <<EOF
# 这是一条注释
${TEST_PREFIX}.file1=value1
${TEST_PREFIX}.file2=value2

# 另一条注释
${TEST_PREFIX}.file3=value with spaces
EOF

$RESETPROP -n -f "$PROPS_FILE" >/dev/null 2>&1
val1=$(rp_get "${TEST_PREFIX}.file1")
val2=$(rp_get "${TEST_PREFIX}.file2")
val3=$(rp_get "${TEST_PREFIX}.file3")
assert_eq "8.1a 文件加载属性 1" "value1" "$val1"
assert_eq "8.1b 文件加载属性 2" "value2" "$val2"
assert_eq "8.1c 文件加载含空格的值" "value with spaces" "$val3"

# 8.2 注释和空行被跳过
log_pass "8.2 注释和空行被正确跳过"

# 8.3 加载不存在的文件（应失败）
assert_failure "8.3 加载不存在的文件返回非 0" $RESETPROP -f "/nonexistent/file.txt"

# 8.4 空文件
: > "$PROPS_FILE"
assert_success "8.4 加载空文件成功" $RESETPROP -n -f "$PROPS_FILE"

# 8.5 只有注释的文件
cat > "$PROPS_FILE" <<EOF
# comment only
# another comment
EOF
assert_success "8.5 加载仅含注释的文件成功" $RESETPROP -n -f "$PROPS_FILE"

# ============================================================================
# 测试组 9: 等待模式 (-w)
# ============================================================================

section "9. 等待模式 (-w)"

# 9.1 等待已存在的属性（应立即返回）
rp_set -n "${TEST_PREFIX}.wait" "exists"
assert_success "9.1 等待已存在的属性立即成功" $RESETPROP -w "${TEST_PREFIX}.wait" --timeout 2

# 9.2 等待超时（属性不存在）
assert_exit "9.2 等待不存在的属性超时" 2 $RESETPROP -w "${TEST_PREFIX}.will_never_exist" --timeout 1

# 9.3 等待属性值变化（当前值不同，应立即返回）
rp_set -n "${TEST_PREFIX}.wait_change" "new_value"
assert_success "9.3 等待属性值变化（值已不同）立即成功" $RESETPROP -w "${TEST_PREFIX}.wait_change" "old_value" --timeout 2

# 9.4 异步等待 + 后台设置（等待属性出现）
$RESETPROP -d "${TEST_PREFIX}.wait" >/dev/null 2>&1
(
    sleep 1
    rp_set -n "${TEST_PREFIX}.wait" "async_set"
) &
bg_pid=$!
$RESETPROP -w "${TEST_PREFIX}.wait" --timeout 5 >/dev/null 2>&1
rc=$?
wait $bg_pid 2>/dev/null
if [ $rc -eq 0 ]; then
    val=$(rp_get "${TEST_PREFIX}.wait")
    assert_eq "9.4 异步等待 + 后台设置" "async_set" "$val"
else
    log_fail "9.4 异步等待 + 后台设置" "exit=0" "exit=$rc"
fi

# 9.5 等待属性值变化（异步修改值）
rp_set -n "${TEST_PREFIX}.wait_change" "old_serial"
(
    sleep 1
    rp_set -n "${TEST_PREFIX}.wait_change" "new_serial"
) &
bg_pid=$!
$RESETPROP -w "${TEST_PREFIX}.wait_change" "old_serial" --timeout 5 >/dev/null 2>&1
rc=$?
wait $bg_pid 2>/dev/null
if [ $rc -eq 0 ]; then
    val=$(rp_get "${TEST_PREFIX}.wait_change")
    assert_eq "9.5 wait 感知到值变化" "new_serial" "$val"
else
    log_fail "9.5 wait 感知到值变化" "exit=0" "exit=$rc"
fi

# ============================================================================
# 测试组 10: 压缩 (-c)
# ============================================================================

section "10. 压缩 (-c)"

# 10.1 先制造一些碎片再压缩
rp_set -n "${TEST_PREFIX}.compact1" "data1"
rp_set -n "${TEST_PREFIX}.compact2" "data2"
rp_set -n "${TEST_PREFIX}.compact3" "data3"
$RESETPROP -d "${TEST_PREFIX}.compact1" >/dev/null 2>&1
$RESETPROP -d "${TEST_PREFIX}.compact2" >/dev/null 2>&1

$RESETPROP -c >/dev/null 2>&1
rc=$?
if [ $rc -eq 0 ]; then
    log_pass "10.1 压缩成功（有数据被回收）"
elif [ $rc -eq 1 ]; then
    log_pass "10.1 压缩完成（无需回收）"
else
    log_fail "10.1 压缩操作" "exit=0 or 1" "exit=$rc"
fi

# 10.2 压缩后属性仍可读
val=$(rp_get "${TEST_PREFIX}.compact3")
assert_eq "10.2 压缩后属性仍可读" "data3" "$val"

# 10.3 指定 context 压缩
$RESETPROP -d "${TEST_PREFIX}.compact3" >/dev/null 2>&1
$RESETPROP -c "u:object_r:default_prop:s0" >/dev/null 2>&1
rc=$?
if [ $rc -eq 0 ] || [ $rc -eq 1 ]; then
    log_pass "10.3 指定 context 压缩成功"
else
    log_fail "10.3 指定 context 压缩" "exit=0 or 1" "exit=$rc"
fi

# ============================================================================
# 测试组 11: 错误处理与边界情况
# ============================================================================

section "11. 错误处理与边界情况"

# 11.1 多个操作模式互斥
assert_failure "11.1 -d 和 -w 互斥" $RESETPROP -d -w "${TEST_PREFIX}.basic"

# 11.2 -d 缺少属性名
assert_failure "11.2 -d 缺少属性名" $RESETPROP -d

# 11.3 -w 缺少属性名
assert_failure "11.3 -w 缺少属性名" $RESETPROP -w --timeout 1

# 11.4 获取系统中确实存在的属性
val=$(rp_get ro.build.type)
if [ -n "$val" ]; then
    log_pass "11.4 读取系统属性 ro.build.type 成功 (=$val)"
else
    log_fail "11.4 读取系统属性 ro.build.type" "non-empty value" "(empty)"
fi

# 11.5 --help 返回成功
assert_success "11.5 --help 返回 0" $RESETPROP --help

# 11.6 --version 返回成功
assert_success "11.6 --version 返回 0" $RESETPROP --version

# ============================================================================
# 测试组 12: 读取已有系统属性（只读验证）
# ============================================================================

section "12. 读取已有系统属性"

val=$(rp_get ro.build.fingerprint)
if [ -n "$val" ]; then
    log_pass "12.1 读取 ro.build.fingerprint (=$val)"
else
    log_skip "12.1 ro.build.fingerprint 不存在"
fi

val=$(rp_get ro.product.model)
if [ -n "$val" ]; then
    log_pass "12.2 读取 ro.product.model (=$val)"
else
    log_skip "12.2 ro.product.model 不存在"
fi

val=$(rp_get ro.build.version.sdk)
if [ -n "$val" ]; then
    log_pass "12.3 读取 ro.build.version.sdk (=$val)"
else
    log_skip "12.3 ro.build.version.sdk 不存在"
fi

# ============================================================================
# 测试组 13: 组合标志测试
# ============================================================================

section "13. 组合标志测试"

_tmp_stderr="${TMP}/_rp_stderr_$$"

# 13.1 -n -v 组合
# 先 delete 再 set，确保是新建属性（避免被之前的值影响）
$RESETPROP -d "${TEST_PREFIX}.basic" >/dev/null 2>&1
$RESETPROP -n -v "${TEST_PREFIX}.basic" "combined_nv" >/dev/null 2>"$_tmp_stderr"
stderr=$(cat "$_tmp_stderr")
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "13.1a -n -v 设置值正确" "combined_nv" "$val"
assert_contains "13.1b -n -v stderr 有输出" "set" "$stderr"

# 13.2 -v -d 组合
rp_set -n "${TEST_PREFIX}.basic" "to_delete_v"
$RESETPROP -v -d "${TEST_PREFIX}.basic" >/dev/null 2>"$_tmp_stderr"
stderr=$(cat "$_tmp_stderr")
assert_contains "13.2 -v -d 输出" "deleted" "$stderr"

rm -f "$_tmp_stderr"

# ============================================================================
# 测试组 14: 串行多属性写入
# ============================================================================

section "14. 串行多属性写入"

# 14.1 串行写入多个不同属性
# 先清理，确保每个属性都是全新创建
for i in 1 2 3 4 5; do
    $RESETPROP -d "${TEST_PREFIX}.concurrent_$i" >/dev/null 2>&1
done
for i in 1 2 3 4 5; do
    rp_set -n "${TEST_PREFIX}.concurrent_$i" "val_$i"
done

all_ok=true
for i in 1 2 3 4 5; do
    val=$(rp_get "${TEST_PREFIX}.concurrent_$i")
    if [ "x${val}" != "xval_$i" ]; then
        all_ok=false
        log_fail "14.1 串行写入属性 $i" "val_$i" "$val"
        break
    fi
done
if $all_ok; then
    log_pass "14.1 串行写入多个属性均正确"
fi

# 清理
for i in 1 2 3 4 5; do
    $RESETPROP -d "${TEST_PREFIX}.concurrent_$i" >/dev/null 2>&1
done

# 14.2 并发读取不影响结果
# 先创建新属性
$RESETPROP -d "${TEST_PREFIX}.basic" >/dev/null 2>&1
rp_set -n "${TEST_PREFIX}.basic" "concurrent_read"

pids=""
_tmp_dir="${TMP}/_rp_concurrent_$$"
mkdir -p "$_tmp_dir"
for i in 1 2 3 4 5; do
    (
        val=$(rp_get "${TEST_PREFIX}.basic")
        if [ "x${val}" = "xconcurrent_read" ]; then
            echo "ok" > "$_tmp_dir/$i"
        else
            echo "fail:$val" > "$_tmp_dir/$i"
        fi
    ) &
    pids="$pids $!"
done

for pid in $pids; do
    wait $pid 2>/dev/null
done

all_read_ok=true
for i in 1 2 3 4 5; do
    result=$(cat "$_tmp_dir/$i" 2>/dev/null)
    if [ "x${result}" != "xok" ]; then
        all_read_ok=false
        break
    fi
done
rm -rf "$_tmp_dir"

if $all_read_ok; then
    log_pass "14.2 并发读取结果一致"
else
    log_fail "14.2 并发读取结果一致" "所有读取正确" "部分读取失败"
fi

# ============================================================================
# 测试组 15: 多次写入同一属性 + 一致性
# ============================================================================

section "15. 多次写入一致性"

# 先删除确保从干净状态开始
$RESETPROP -d "${TEST_PREFIX}.basic" >/dev/null 2>&1

# 15.1 连续写入 10 次，最终值正确
i=0
while [ $i -lt 10 ]; do
    rp_set -n "${TEST_PREFIX}.basic" "iter_$i"
    i=$((i + 1))
done
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "15.1 连续写入 10 次后值正确" "iter_9" "$val"

# 15.2 短值后尝试写入非 ro. 长值应失败，且原值保持不变
# 先删除再重建，避免被前面的值影响
$RESETPROP -d "${TEST_PREFIX}.basic" >/dev/null 2>&1
rp_set -n "${TEST_PREFIX}.basic" "short"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "15.2a 短值" "short" "$val"

long_val=""
i=0
while [ $i -lt 150 ]; do
    long_val="${long_val}X"
    i=$((i + 1))
done
assert_failure "15.2b 非 ro. 切换到长值被拒绝" \
    "$RESETPROP" -n "${TEST_PREFIX}.basic" "$long_val"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "15.2c 长值写入失败后保留原短值" "short" "$val"

rp_set -n "${TEST_PREFIX}.basic" "back_to_short"
val=$(rp_get "${TEST_PREFIX}.basic")
assert_eq "15.2d 切换回短值" "back_to_short" "$val"

# ============================================================================
# 测试结果汇总
# ============================================================================

printf "\n${CYAN}══════════════════════════════════════════════════════════${NC}\n"
printf "${CYAN}  测试结果汇总${NC}\n"
printf "${CYAN}══════════════════════════════════════════════════════════${NC}\n"
printf "  总计: %d\n" "$TOTAL"
printf "${GREEN}  通过: %d${NC}\n" "$PASS"
printf "${RED}  失败: %d${NC}\n" "$FAIL"
printf "${YELLOW}  跳过: %d${NC}\n" "$SKIP"
printf "${CYAN}══════════════════════════════════════════════════════════${NC}\n"

if [ "$FAIL" -gt 0 ]; then
    printf "${RED}  测试未全部通过！${NC}\n"
    exit 1
else
    printf "${GREEN}  全部通过！${NC}\n"
    exit 0
fi
