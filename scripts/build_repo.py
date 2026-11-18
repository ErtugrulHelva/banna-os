#!/usr/bin/env python3
"""packages/ altındaki paket tanımlarından hatch deposu üretir.

Her paket: packages/<ad>/package.ini (version, description, depends, bin, app) ve
isteğe bağlı packages/<ad>/files/ ağacı (kök dizine göre yollar).
`app = <kimlik>`: apps/<kimlik>/ masaüstü uygulaması (app.conf, program, PNG'ler)
/apps/<ad>/ altına paketlenir; böyle uygulamalar diske konmaz, yalnızca Store'dan kurulur.
Sürüm notları: packages/<ad>/CHANGELOG.md, "## <sürüm>" (isteğe bağlı " (YYYY-AA-GG)") başlığı altında
"- madde" satırları. Paketin sürümü için bölüm yoksa derleme durur. package.ini'deki `about`
Banna Store'un uygulama sayfasındaki tanıtım metnidir.
Çıktı: <çıktı>/index ve <çıktı>/pkgs/<ad>-<sürüm>.hpk
"""
import hashlib
import pathlib
import re
import subprocess
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
PACKAGES = ROOT / "packages"
USER_BIN = ROOT / "user/target/x86_64-banna_user/release"
APPS = ROOT / "apps"
ALLOWED = ("/bin/", "/usr/")


def read_ini(path):
    meta = {}
    for line in path.read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        key, _, value = line.partition("=")
        meta[key.strip()] = value.strip()
    return meta


def changelog(path, version):
    """CHANGELOG.md'de `version` bölümü: (tarih ya da "", maddeler) — dosya ya da bölüm yoksa None."""
    if not path.is_file():
        return None
    header = re.compile(r"^## +(\S+)(?: +\((\d{4}-\d{2}-\d{2})\))? *$")
    found, date, items = False, "", []
    for line in path.read_text(encoding="utf-8").splitlines():
        m = header.match(line)
        if m:
            if found:
                break
            found = m.group(1) == version
            date = m.group(2) or ""
        elif found and line.startswith("- "):
            items.append(line[2:].strip())
    return (date, items) if found else None


def one_line(text):
    return " ".join(text.split())


def package_files(name, directory, meta):
    files = []
    for binary in meta.get("bin", "").split():
        source = USER_BIN / binary
        if not source.is_file():
            sys.exit(f"{name}: {source} yok (önce: make user)")
        files.append((f"/bin/{binary}", source.read_bytes()))
    app = meta.get("app")
    if app:
        source = APPS / app
        binary = USER_BIN / app
        if not (source / "app.conf").is_file():
            sys.exit(f"{name}: {source}/app.conf yok")
        if not binary.is_file():
            sys.exit(f"{name}: {binary} yok (önce: make user)")
        files.append((f"/apps/{name}/app.conf", (source / "app.conf").read_bytes()))
        files.append((f"/apps/{name}/{name}", binary.read_bytes()))
        for png in sorted(source.glob("*.png")):
            files.append((f"/apps/{name}/{png.name}", png.read_bytes()))
    tree = directory / "files"
    if tree.is_dir():
        for source in sorted(tree.rglob("*")):
            if source.is_file() and source.name != ".DS_Store":
                files.append(("/" + source.relative_to(tree).as_posix(), source.read_bytes()))
    for path, _ in files:
        if not (
            path.startswith(ALLOWED)
            or path.startswith(f"/etc/{name}/")
            or path.startswith(f"/apps/{name}/")
        ):
            sys.exit(f"{name}: izin verilmeyen yol {path}")
    return files


def build(out):
    (out / "pkgs").mkdir(parents=True, exist_ok=True)
    entries = []
    produced = set()
    names = sorted(d.name for d in PACKAGES.iterdir() if (d / "package.ini").is_file())
    for name in names:
        directory = PACKAGES / name
        meta = read_ini(directory / "package.ini")
        version = meta.get("version") or sys.exit(f"{name}: version eksik")
        archive = bytearray(b"HPK1\n")
        for path, data in package_files(name, directory, meta):
            archive += f"F {len(data)} {path}\n".encode() + data
        file = f"pkgs/{name}-{version}.hpk"
        (out / file).write_bytes(archive)
        produced.add(file)
        depends = ",".join(d.strip() for d in meta.get("depends", "").split(",") if d.strip())
        for dep in filter(None, depends.split(",")):
            if dep not in names:
                sys.exit(f"{name}: bağımlılık {dep} packages/ içinde yok")
        # Masaüstü uygulaması paketleri: Banna Store adı ve simgeyi app.conf'tan gösterir.
        conf = APPS / meta["app"] / "app.conf" if meta.get("app") else directory / "files/apps" / name / "app.conf"
        app = read_ini(conf) if conf.is_file() else {}
        extra = ""
        if app:
            extra = f"title {app.get('name', name)}\nicon {app.get('icon', '')}\n"
        if meta.get("builder"):
            extra += f"builder {meta['builder']}\n"
        if meta.get("about"):
            extra += f"about {one_line(meta['about'])}\n"
        notes = changelog(directory / "CHANGELOG.md", version)
        if (directory / "CHANGELOG.md").is_file() and notes is None:
            sys.exit(f"{name}: CHANGELOG.md'de '## {version}' bölümü yok (sürüm notunu yazın)")
        if notes:
            if notes[0]:
                extra += f"date {notes[0]}\n"
            extra += "".join(f"note {one_line(item)}\n" for item in notes[1])
        entries.append(
            f"package {name}\nversion {version}\nsize {len(archive)}\n"
            f"sha256 {hashlib.sha256(archive).hexdigest()}\ndepends {depends}\n"
            f"file {file}\ndescription {meta.get('description', '')}\n{extra}end\n"
        )
        print(f"  {name} {version} ({len(archive)} bayt)")
    for stale in (out / "pkgs").glob("*.hpk"):
        if f"pkgs/{stale.name}" not in produced:
            stale.unlink()
    (out / "index").write_text("hatch-index 1\n" + "".join(entries), encoding="utf-8")
    # hatch, listeyi gömülü açık anahtarla doğrular (bkz. user/src/ecosystem.rs).
    key = ROOT / "keys/hatch-signing.pem"
    if not key.is_file():
        sys.exit(f"imza anahtarı yok: {key} (depo imzalanmadan hatch kabul etmez)")
    subprocess.run(
        ["openssl", "dgst", "-sha256", "-sign", str(key), "-out", str(out / "index.sig"), str(out / "index")],
        check=True,
    )
    print(f"depo: {out} ({len(entries)} paket)")


if __name__ == "__main__":
    build(pathlib.Path(sys.argv[1] if len(sys.argv) > 1 else ROOT / "build/repo"))
