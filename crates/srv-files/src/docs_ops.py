# srv-files F-3: the document operations, following the installed skills' documented
# recipes (pdf: pypdf to read, reportlab platypus to write; xlsx: openpyxl; pptx:
# python-pptx). Run by crates/srv-files/src/docs.rs as `python -c <this file>` with one
# JSON request on stdin; prints one JSON reply on stdout. No network, no shell.
import json
import sys


def reply(obj):
    sys.stdout.write(json.dumps(obj))
    sys.stdout.flush()


def need(module, pip_name=None):
    try:
        return __import__(module, fromlist=["_"])
    except Exception:
        raise Missing(pip_name or module)


class Missing(Exception):
    pass


def page_range(spec, n):
    if not spec:
        return list(range(n))
    out = []
    for part in str(spec).split(","):
        part = part.strip()
        if not part:
            continue
        if "-" in part:
            a, b = part.split("-", 1)
            a = int(a) if a.strip() else 1
            b = int(b) if b.strip() else n
        else:
            a = b = int(part)
        for p in range(max(a, 1), min(b, n) + 1):
            out.append(p - 1)
    return out


def pdf_read(req):
    pypdf = need("pypdf")
    reader = pypdf.PdfReader(req["path"])
    n = len(reader.pages)
    parts = []
    for i in page_range(req.get("pages"), n):
        text = reader.pages[i].extract_text() or ""
        parts.append("page %d of %d\n%s" % (i + 1, n, text.strip()))
    return {"text": "\n\n".join(parts), "pages": n}


def esc(s):
    return s.replace("&", "&amp;").replace("<", "&lt;").replace(">", "&gt;")


def inline(s):
    # **bold** and *italic* after escaping; anything else stays text.
    import re

    s = esc(s)
    s = re.sub(r"\*\*(.+?)\*\*", r"<b>\1</b>", s)
    s = re.sub(r"(?<![*\w])\*(?!\s)(.+?)(?<!\s)\*(?![*\w])", r"<i>\1</i>", s)
    s = re.sub(r"`([^`]+)`", r"<font face='Courier'>\1</font>", s)
    return s


def render_pdf(out, markdown, title=None):
    need("reportlab")
    from reportlab.lib.pagesizes import A4
    from reportlab.lib.styles import getSampleStyleSheet
    from reportlab.platypus import Paragraph, SimpleDocTemplate, Spacer, Preformatted

    styles = getSampleStyleSheet()
    story = []
    if title:
        story.append(Paragraph(inline(title), styles["Title"]))
    para = []
    code = None

    def flush():
        if para:
            story.append(Paragraph(inline(" ".join(para)), styles["BodyText"]))
            del para[:]

    for raw in markdown.splitlines():
        line = raw.rstrip()
        if code is not None:
            if line.strip().startswith("```"):
                story.append(Preformatted("\n".join(code), styles["Code"]))
                code = None
            else:
                code.append(raw)
            continue
        stripped = line.strip()
        if stripped.startswith("```"):
            flush()
            code = []
        elif not stripped:
            flush()
            story.append(Spacer(1, 4))
        elif stripped.startswith("### "):
            flush()
            story.append(Paragraph(inline(stripped[4:]), styles["Heading3"]))
        elif stripped.startswith("## "):
            flush()
            story.append(Paragraph(inline(stripped[3:]), styles["Heading2"]))
        elif stripped.startswith("# "):
            flush()
            story.append(Paragraph(inline(stripped[2:]), styles["Heading1"]))
        elif stripped[:2] in ("- ", "* ", "+ "):
            flush()
            story.append(Paragraph(inline(stripped[2:]), styles["BodyText"], bulletText="•"))
        else:
            para.append(stripped)
    flush()
    if code is not None:
        story.append(Preformatted("\n".join(code), styles["Code"]))
    if not story:
        story.append(Spacer(1, 4))
    doc = SimpleDocTemplate(out, pagesize=A4, title=title or "")
    doc.build(story)
    return {"text": "", "files": [out]}


def pdf_write(req):
    return render_pdf(req["out"], req.get("text", ""), req.get("title"))


def md_to_pdf(req):
    with open(req["path"], "r", encoding="utf-8", errors="replace") as f:
        md = f.read()
    return render_pdf(req["out"], md, req.get("title"))


def cell_text(v):
    if v is None:
        return ""
    if isinstance(v, float) and v.is_integer():
        return str(int(v))
    return str(v)


def xlsx_sheets(req):
    openpyxl = need("openpyxl")
    wb = openpyxl.load_workbook(req["path"], read_only=True, data_only=True)
    names = wb.sheetnames
    want = req.get("sheet")
    if want:
        if want not in names:
            raise ValueError("no sheet %r; sheets: %s" % (want, ", ".join(names)))
        names = [want]
    return wb, names


def xlsx_read(req):
    wb, names = xlsx_sheets(req)
    max_rows = int(req.get("max_rows") or 200)
    parts = []
    for name in names:
        ws = wb[name]
        lines = ["## " + name]
        total = 0
        for row in ws.iter_rows(values_only=True):
            total += 1
            if total <= max_rows:
                lines.append(" | ".join(cell_text(v) for v in row))
        if total > max_rows:
            lines.append("(%d of %d rows shown)" % (max_rows, total))
        parts.append("\n".join(lines))
    return {"text": "\n\n".join(parts), "sheets": wb.sheetnames}


def xlsx_rows(req):
    wb, names = xlsx_sheets(req)
    ws = wb[names[0]]
    rows = [[cell_text(v) for v in row] for row in ws.iter_rows(values_only=True)]
    header = rows[0] if rows else []
    return {"header": header, "rows": rows[1:], "sheet": names[0]}


def xlsx_write(req):
    openpyxl = need("openpyxl")
    from openpyxl.styles import Font

    wb = openpyxl.Workbook()
    wb.remove(wb.active)
    formulas = False
    sheets = req.get("sheets") or []
    if not sheets:
        raise ValueError("no sheets given")
    for sheet in sheets:
        ws = wb.create_sheet(title=str(sheet.get("name") or "Sheet%d" % (len(wb.sheetnames) + 1))[:31])
        widths = {}
        for r, row in enumerate(sheet.get("rows") or [], start=1):
            for c, v in enumerate(row, start=1):
                if isinstance(v, str) and v.startswith("="):
                    formulas = True
                ws.cell(row=r, column=c, value=v)
                widths[c] = max(widths.get(c, 0), len(cell_text(v)))
        if sheet.get("header", True) and ws.max_row >= 1:
            for cell in ws[1]:
                cell.font = Font(bold=True)
            ws.freeze_panes = "A2"
        for c, w in widths.items():
            ws.column_dimensions[openpyxl.utils.get_column_letter(c)].width = min(max(w + 2, 8), 60)
    wb.save(req["out"])
    return {"text": "", "files": [req["out"]], "formulas": formulas}


def pptx_write(req):
    pptx = need("pptx", "python-pptx")
    prs = pptx.Presentation()
    if req.get("title"):
        s = prs.slides.add_slide(prs.slide_layouts[0])
        s.shapes.title.text = req["title"]
        if req.get("subtitle") and len(s.placeholders) > 1:
            s.placeholders[1].text = req["subtitle"]
    slides = req.get("slides") or []
    if not slides and not req.get("title"):
        raise ValueError("no slides given")
    for sl in slides:
        s = prs.slides.add_slide(prs.slide_layouts[1])
        s.shapes.title.text = str(sl.get("title") or "")
        body = s.placeholders[1].text_frame
        bullets = sl.get("bullets") or []
        for i, b in enumerate(bullets):
            p = body.paragraphs[0] if i == 0 else body.add_paragraph()
            p.text = str(b)
        if sl.get("notes"):
            s.notes_slide.notes_text_frame.text = str(sl["notes"])
    prs.save(req["out"])
    return {"text": "", "files": [req["out"]]}


OPS = {
    "pdf.read": pdf_read,
    "pdf.write": pdf_write,
    "md.to_pdf": md_to_pdf,
    "xlsx.read": xlsx_read,
    "xlsx.rows": xlsx_rows,
    "xlsx.write": xlsx_write,
    "pptx.write": pptx_write,
}


def main():
    try:
        req = json.loads(sys.stdin.buffer.read().decode("utf-8"))
        op = OPS[req["op"]]
    except Exception as e:
        reply({"ok": False, "error": "bad request: %s" % e})
        return
    try:
        out = op(req)
        out["ok"] = True
        reply(out)
    except Missing as m:
        reply({"ok": False, "missing": str(m)})
    except Exception as e:
        reply({"ok": False, "error": "%s: %s" % (type(e).__name__, e)})


main()
