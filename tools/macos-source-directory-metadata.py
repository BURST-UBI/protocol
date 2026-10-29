"""Restore directory times implied by a verified, file-only frozen archive."""
import hashlib
import json
import os
from pathlib import Path
import tarfile

root = Path(__file__).resolve().parent.parent
source = root / 'candidate-source'
manifest = json.loads((root / 'validation-inputs/source-manifest.json').read_text())
archive_path = root / 'validation-inputs/source.tar.gz'
assert hashlib.sha256(archive_path.read_bytes()).hexdigest() == manifest['archiveSha256']
actual = {str(p.relative_to(source)): hashlib.sha256(p.read_bytes()).hexdigest()
          for p in source.rglob('*') if p.is_file()}
assert actual == manifest['files'], 'Only an exact frozen source tree can reuse archive times'
times = {}
with tarfile.open(archive_path) as archive:
    for member in archive:
        assert member.isfile() and member.name in manifest['files']
        for parent in Path(member.name).parents:
            times[parent] = max(times.get(parent, member.mtime), member.mtime)
for parent in sorted(times, key=lambda p: len(p.parts), reverse=True):
    path = source / parent
    assert path.is_dir() and not path.is_symlink()
    os.utime(path, (times[parent], times[parent]))
print('Verified frozen files; restored', len(times), 'directory timestamps. No contents or build outputs changed.')
