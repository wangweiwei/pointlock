#!/usr/bin/env bash
# 本地发布脚本（零外部依赖：bash + git + curl + python3 + cargo）。
#
# 用法:
#   scripts/release.sh publish            # 交互选择 current / major / minor / patch
#   scripts/release.sh publish patch      # 非交互（stdin 非终端时只能用这种形式）
#   scripts/release.sh prepare minor      # 只改文件不提交，留给人工审阅/补 CHANGELOG
#   scripts/release.sh status [X.Y.Z]     # 查 crates.io + npm 的真实发布状态（默认当前版本）
#
# publish 流程：预检（分支/干净树/tag 不存在/registry 未占用）→ 改写全部版本触点
# （6 个 package.json、根 Cargo.toml 的 workspace 版本 + 9 个内部钉子、CHANGELOG
# 新节与链接尾注）→ cargo metadata 刷新 Cargo.lock → commit → push 分支 →
# 打注解 tag → push tag。push tag 触发 .github/workflows/release.yml 自动发布。
# 版本号一律由 bump 推导，不手输；registry 版本不可变，烧掉的号不再复用。
#
# `current` 只在当前版本从未打过 tag 时可选：不改任何文件，直接对 HEAD 打 tag
# （首发场景——树里已是待发版本）。
#
# 环境变量:
#   RELEASE_BRANCH   允许发布的分支（默认 main）
set -euo pipefail

cd "$(dirname "$0")/.."

REPO_URL="https://github.com/wangweiwei/pointlock"
BRANCH="${RELEASE_BRANCH:-main}"
INTERNAL_PINS=9
CRATES=(pointlock-ir pointlock-expr pointlock-provider-kit pointlock-vision
	pointlock-store pointlock-compiler pointlock-human-cli pointlock-runner
	pointlock-provider-devicerail pointlock-cli)
NPM_PACKAGES=(@pointlock/projection-types @pointlock/nl-drafter)
MANIFESTS=(package.json packages/ir-types/package.json
	packages/nl-drafter/package.json packages/projection-types/package.json
	packages/ui/package.json packages/walk-drafter/package.json)

info() { printf '\033[1;34m==>\033[0m %s\n' "$*"; }
die()  { printf '\033[1;31merror:\033[0m %s\n' "$*" >&2; exit 1; }

current_version() {
	grep -m1 '^version = ' Cargo.toml | sed -E 's/version = "(.*)"/\1/'
}

bump_version() { # bump_version <current> <major|minor|patch>
	local cur_major cur_minor cur_patch
	IFS='.' read -r cur_major cur_minor cur_patch <<<"$1"
	case "$2" in
	major) echo "$((cur_major + 1)).0.0" ;;
	minor) echo "$cur_major.$((cur_minor + 1)).0" ;;
	patch) echo "$cur_major.$cur_minor.$((cur_patch + 1))" ;;
	*) die "非法的 bump 类型: $2" ;;
	esac
}

# ---- registry 查询（curl 失败或响应异常一律按 missing 处理；真正的守门在
# ---- CI 的发布循环里，它对已存在版本会跳过而不是失败）----
crates_has() { # crates_has <crate> <version>
	curl --silent --header 'User-Agent: Pointlock release script' \
		"https://crates.io/api/v1/crates/$1" 2>/dev/null |
		python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
sys.exit(0 if sys.argv[1] in [v["num"] for v in d.get("versions",[])] else 1)' "$2" 2>/dev/null
}

npm_has() { # npm_has <package> <version>
	curl --silent "https://registry.npmjs.org/$1" 2>/dev/null |
		python3 -c 'import sys,json
try: d=json.load(sys.stdin)
except Exception: sys.exit(1)
sys.exit(0 if sys.argv[1] in d.get("versions",{}) else 1)' "$2" 2>/dev/null
}

print_status() { # print_status <version> ；发布过任何一个包则返回 1
	local ver="$1" missing=0
	for c in "${CRATES[@]}"; do
		if crates_has "$c" "$ver"; then
			printf '  published  crates.io  %s\n' "$c"
		else
			printf '  missing    crates.io  %s\n' "$c"
			missing=$((missing + 1))
		fi
	done
	for p in "${NPM_PACKAGES[@]}"; do
		if npm_has "$p" "$ver"; then
			printf '  published  npm        %s\n' "$p"
		else
			printf '  missing    npm        %s\n' "$p"
			missing=$((missing + 1))
		fi
	done
	echo "$missing"
}

require_unpublished() { # require_unpublished <version>
	for c in "${CRATES[@]}"; do
		crates_has "$c" "$1" && die "$1 已发布在 crates.io（${c}），registry 版本不可变，请换一个 bump"
	done
	for p in "${NPM_PACKAGES[@]}"; do
		npm_has "$p" "$1" && die "$1 已发布在 npm（${p}），registry 版本不可变，请换一个 bump"
	done
	return 0
}

require_clean() {
	[ -z "$(git status --porcelain)" ] || die "工作区有未提交/未追踪改动，请先提交或清理"
}

require_branch() {
	[ "$(git rev-parse --abbrev-ref HEAD)" = "$BRANCH" ] ||
		die "当前不在 $BRANCH 分支（设 RELEASE_BRANCH 可覆盖）"
}

require_absent_tag() { # require_absent_tag <tag>
	[ -z "$(git tag --list "$1")" ] || die "tag $1 已存在于本地；registry 版本不可变"
	[ -z "$(git ls-remote --tags origin "refs/tags/$1")" ] || die "tag $1 已存在于 origin；registry 版本不可变"
}

tag_absent() { # tag_absent <tag>（不终止，只返回真假）
	[ -z "$(git tag --list "$1")" ] && [ -z "$(git ls-remote --tags origin "refs/tags/$1")" ]
}

# ---- 版本触点改写。任何一步失败都整体回滚（进入前已证明树是干净的，
# ---- 此刻的脏文件只可能是本脚本自己的半成品）----
apply_version() { # apply_version <prev> <new>
	if ! rewrite_all "$1" "$2"; then
		git checkout -- .
		die "版本改写失败，已回滚全部改动"
	fi
}

rewrite_all() { # rewrite_all <prev> <new>
	local prev="$1" new="$2" m count

	for m in "${MANIFESTS[@]}"; do
		count="$(grep -cE '^  "version": "[^"]+",$' "$m" || true)"
		[ "$count" = "1" ] || { echo "$m: 应恰好有 1 行 version，实为 $count" >&2; return 1; }
		sed -i.bak -E "s/^  \"version\": \"[^\"]+\",$/  \"version\": \"$new\",/" "$m"
		rm -f "$m.bak"
	done

	# workspace 版本（限定在 [workspace.package] 段内）
	sed -i.bak "/^\[workspace.package\]/,/^\[/ s/^version = \".*\"/version = \"$new\"/" Cargo.toml
	rm -f Cargo.toml.bak
	[ "$(current_version)" = "$new" ] || { echo "Cargo.toml workspace 版本写入失败" >&2; return 1; }

	# 内部 path 依赖的 version 钉子：只 bump workspace 版本的话，钉子落在旧版本
	# 上，^旧版 不满足新版，整个 workspace 连依赖解析都过不去。两处必须同步。
	# devicerail-client 的钉子跟随其自身发布节奏，特意不在此模式覆盖内。
	sed -i.bak -E "s|^(pointlock-[a-z-]+ = \{ path = \"crates/pointlock-[a-z-]+\", version = )\"[^\"]*\"|\1\"$new\"|" Cargo.toml
	rm -f Cargo.toml.bak
	count="$(grep -cE "^pointlock-[a-z-]+ = \{ path = \"crates/pointlock-[a-z-]+\", version = \"$new\"" Cargo.toml || true)"
	[ "$count" = "$INTERNAL_PINS" ] || { echo "内部依赖钉子应为 $INTERNAL_PINS 处，实写成 $count 处" >&2; return 1; }

	# CHANGELOG：新节插在第一个已发布版本节之前（保住 [Unreleased] 下的正文），
	# 并维护底部链接（[Unreleased] 对比基线 + 新版本对比链接）。
	NEW="$new" PREV="$prev" REPO="$REPO_URL" python3 - <<'PY' || return 1
import datetime, os, re
new, prev, repo = os.environ["NEW"], os.environ["PREV"], os.environ["REPO"]
date = datetime.date.today().isoformat()
s = open("CHANGELOG.md", encoding="utf-8").read()
heads = list(re.finditer(r"^## \[(?!Unreleased\])", s, re.M))
assert len(heads) >= 1, "CHANGELOG.md 里找不到已发布版本节"
i = heads[0].start()
s = s[:i] + f"## [{new}] — {date}\n\n" + s[i:]
assert len(re.findall(r"^\[Unreleased\]: ", s, re.M)) == 1, "CHANGELOG.md 链接尾注异常"
s = re.sub(
    r"^\[Unreleased\]: .*$",
    f"[Unreleased]: {repo}/compare/v{new}...HEAD\n[{new}]: {repo}/compare/v{prev}...v{new}",
    s, count=1, flags=re.M,
)
open("CHANGELOG.md", "w", encoding="utf-8").write(s)
PY

	# 刷新 Cargo.lock 里 10 个 workspace 成员的版本，并顺带证明 workspace 可解析。
	cargo metadata --format-version 1 --quiet >/dev/null || { echo "版本写入后 workspace 无法解析" >&2; return 1; }
}

choose_release() { # choose_release <current> ；stdout 输出选中的 bump
	local current="$1" options=() o i=1 ans
	[ -t 0 ] || die "stdin 非终端时必须显式指定 bump：current / major / minor / patch"
	tag_absent "v$current" && options+=(current)
	options+=(major minor patch)
	printf 'current release %s\n\n' "$current" >&2
	for o in "${options[@]}"; do
		if [ "$o" = "current" ]; then
			printf '  %d  %-7s  %s  (unreleased)\n' "$i" "$o" "$current" >&2
		else
			printf '  %d  %-7s  %s\n' "$i" "$o" "$(bump_version "$current" "$o")" >&2
		fi
		i=$((i + 1))
	done
	printf '\n' >&2
	read -r -p "release [1-${#options[@]}]: " ans || die "已取消选择"
	[[ "$ans" =~ ^[0-9]+$ ]] && [ "$ans" -ge 1 ] && [ "$ans" -le "${#options[@]}" ] ||
		die "release 应为 1 到 ${#options[@]} 之间的数字"
	echo "${options[$((ans - 1))]}"
}

cmd_publish() { # cmd_publish [bump]
	require_branch
	require_clean
	local current bump new tag
	current="$(current_version)"
	bump="${1:-$(choose_release "$current")}"

	if [ "$bump" = "current" ]; then
		tag="v$current"
		tag_absent "$tag" || die "$current 已打过 tag，请选 major / minor / patch"
		require_unpublished "$current"
		info "首发当前版本 ${current}：不改文件，直接对 HEAD 打 tag $tag"
		git push origin "$BRANCH"
		git tag -a "$tag" -m "$tag"
		git push origin "$tag"
		info "已 push ${tag}，release.yml 将开始自动发布"
		return
	fi

	case "$bump" in major | minor | patch) ;; *) die "bump 应为 current / major / minor / patch" ;; esac
	new="$(bump_version "$current" "$bump")"
	tag="v$new"
	require_absent_tag "$tag"
	require_unpublished "$new"
	info "当前版本 $current  →  目标版本 $new  (tag $tag)"
	apply_version "$current" "$new"
	git commit --all --quiet --message "release: $tag"
	info "已提交版本改写"
	git push origin "$BRANCH"
	git tag -a "$tag" -m "$tag"
	git push origin "$tag"
	info "已 push ${tag}，release.yml 将开始自动发布"
}

cmd_prepare() { # cmd_prepare [bump]
	require_branch
	require_clean
	local current bump new
	current="$(current_version)"
	bump="${1:-$(choose_release "$current")}"
	if [ "$bump" = "current" ]; then
		info "树里已是未发布的 ${current}，无需改写；发布执行: scripts/release.sh publish current"
		return
	fi
	case "$bump" in major | minor | patch) ;; *) die "bump 应为 current / major / minor / patch" ;; esac
	new="$(bump_version "$current" "$bump")"
	require_absent_tag "v$new"
	require_unpublished "$new"
	apply_version "$current" "$new"
	git diff --name-only | sed 's/^/  /'
	info "已改写为 ${new}（未提交）：补好 CHANGELOG 正文并提交后，打 tag v$new 推送即发布"
}

cmd_status() { # cmd_status [version]
	local ver="${1:-$(current_version)}" missing rows
	[[ "$ver" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "status 的参数应为 X.Y.Z 形式的版本号"
	rows="$(print_status "$ver")"
	missing="$(printf '%s\n' "$rows" | tail -1)"
	printf '%s\n' "$rows" | sed '$d'
	if [ "$missing" = "0" ]; then
		info "every public package is published at $ver"
	else
		printf '\n%s\n' "有 $missing 个包缺失。到 Actions 页对 tag v$ver 重跑 Release 工作流即可补发：" >&2
		printf '%s\n' "发布循环对 registry 已有的包会跳过，重跑是安全的。" >&2
	fi
}

case "${1:-}" in
publish) cmd_publish "${2:-}" ;;
prepare) cmd_prepare "${2:-}" ;;
status)  cmd_status  "${2:-}" ;;
*) die "用法: $0 <publish|prepare|status> [current|major|minor|patch|X.Y.Z]" ;;
esac
