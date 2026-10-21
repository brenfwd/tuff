import re, sys, json, pathlib
root = pathlib.Path(sys.argv[1])
lit = r'"((?:[^"\\]|\\.)*)"'
def unesc(s):
    return re.sub(r'\\(u[0-9a-fA-F]{4}|.)', lambda m: chr(int(m.group(1)[1:],16)) if m.group(1)[0]=='u' else {'n':'\n','t':'\t','"':'"','\\':'\\',"'":"'"}.get(m.group(1), '\\'+m.group(1)), s)
out=[]; seen=set()
files = sorted((root/'tag/standard').glob('*.java')) + [root/'MiniMessageParserTest.java', root/'MiniMessageTest.java']
for f in files:
    src=f.read_text(); test=None
    for line in src.splitlines():
        m=re.search(r'void (\w+)\(', line)
        if m: test=m.group(1)
        cands=[]
        m=re.search(r'String input\w* = '+lit+r'\s*;', line)
        if m: cands.append(m.group(1))
        m=re.search(r'assertParsedEquals\([^"]*'+lit+r'\s*\)\s*;', line)
        if m: cands.append(m.group(1))
        for c in cands:
            s=unesc(c)
            if s in seen: continue
            seen.add(s); out.append({"source": f"{f.stem}.{test}", "input": s})
json.dump(out, sys.stdout, indent=1, ensure_ascii=True)
