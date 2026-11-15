"""Reproduce Komando's embedded Devicon catalog from a pinned upstream revision."""
import io
import json
import pathlib
import urllib.request
import zipfile

REVISION = '32fe1d3b3f36ad11f90ab94f7db59738139c2bf6'
ROOT = pathlib.Path(__file__).resolve().parents[1]

def main():
    with urllib.request.urlopen(f'https://codeload.github.com/devicons/devicon/zip/{REVISION}', timeout=60) as response:
        archive = zipfile.ZipFile(io.BytesIO(response.read()))
    prefix = f'devicon-{REVISION}/'
    catalog = json.loads(archive.read(prefix + 'devicon.json'))
    destination = ROOT / 'assets/devicon'
    destination.mkdir(parents=True, exist_ok=True)
    entries = []
    for item in sorted(catalog, key=lambda item: item['name']):
        variant = next((v for v in ('original', 'plain', 'line') if v in item['versions']['svg']), None)
        if variant is None:
            continue
        name = item['name']
        member = f'{prefix}icons/{name}/{name}-{variant}.svg'
        if member not in archive.namelist():
            continue
        (destination / f'{name}.svg').write_bytes(archive.read(member))
        search = ' '.join([name, *item.get('altnames', []), *item.get('tags', [])])
        search += {'kubernetes': ' k8s', 'redhat': ' rhel red-hat', 'powershell': ' pwsh windows-powershell'}.get(name, '')
        entries.append(f'    BrandIcon {{ id: "devicon:{name}", search: {json.dumps(search, ensure_ascii=False)}, svg: include_bytes!("../../assets/devicon/{name}.svg"), image: OnceLock::new() }},')
    (destination / 'LICENSE').write_bytes(archive.read(prefix + 'LICENSE'))
    source = '// Generated catalog of unmodified Devicon SVGs; see assets/devicon/README.md.\nuse super::BrandIcon;\nuse std::sync::OnceLock;\n'
    source += f'pub static ICONS: [BrandIcon; {len(entries)}] = [\n' + '\n'.join(entries) + '\n];\n'
    (ROOT / 'src/icons/catalog.rs').write_text(source, encoding='utf-8', newline='\n')
    print(f'Bundled {len(entries)} Devicon icons at {REVISION}')

if __name__ == '__main__':
    main()
