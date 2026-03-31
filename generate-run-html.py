#!/usr/bin/env python3
"""
generate-run-html.py

Converts run.md to a styled HTML document with Google-inspired theme:
- Roboto font family (Google's signature font)
- Material Design color palette
- Clean, modern styling
- Dark/Light theme support
- Responsive design

Usage:
    python3 generate-run-html.py [input.md] [output.html]

If 'markdown' package is available, uses it for better conversion.
Otherwise, uses built-in regex-based conversion.
"""

import sys
import re
from pathlib import Path

# Try to import markdown library
try:
    import markdown
    from markdown.extensions.toc import TocExtension
    from markdown.extensions.fenced_code import FencedCodeExtension
    from markdown.extensions.tables import TableExtension
    HAS_MARKDOWN = True
except ImportError:
    HAS_MARKDOWN = False
    print("Note: 'markdown' package not found. Using built-in converter.")
    print("For better results: pip install markdown")
    print()


def convert_markdown_builtin(md_text: str) -> str:
    """Convert markdown to HTML using regex (fallback)."""

    html = md_text

    # Escape HTML in code blocks first (preserve them)
    code_blocks = []
    def save_code_block(match):
        code_blocks.append(match.group(0))
        return f"__CODE_BLOCK_{len(code_blocks) - 1}__"

    # Save fenced code blocks
    html = re.sub(r'```[\s\S]*?```', save_code_block, html)

    # Save inline code
    inline_codes = []
    def save_inline_code(match):
        inline_codes.append(match.group(1))
        return f"__INLINE_CODE_{len(inline_codes) - 1}__"
    html = re.sub(r'`([^`]+)`', save_inline_code, html)

    # Headers
    html = re.sub(r'^# (.+)$', r'<h1>\1</h1>', html, flags=re.MULTILINE)
    html = re.sub(r'^## (\d+)\. (.+)$',
                  lambda m: f'<h2 id="{m.group(2).lower().replace(" ", "-").replace("&", "").replace("/", "-")}">{m.group(1)}. {m.group(2)}</h2>',
                  html, flags=re.MULTILINE)
    html = re.sub(r'^## (.+)$',
                  lambda m: f'<h2 id="{m.group(1).lower().replace(" ", "-").replace("&", "").replace("/", "-")}">{m.group(1)}</h2>',
                  html, flags=re.MULTILINE)
    html = re.sub(r'^### (.+)$', r'<h3>\1</h3>', html, flags=re.MULTILINE)
    html = re.sub(r'^#### (.+)$', r'<h4>\1</h4>', html, flags=re.MULTILINE)

    # Bold and italic
    html = re.sub(r'\*\*([^*]+)\*\*', r'<strong>\1</strong>', html)
    html = re.sub(r'\*([^*]+)\*', r'<em>\1</em>', html)

    # Links
    html = re.sub(r'\[([^\]]+)\]\(([^)]+)\)', r'<a href="\2">\1</a>', html)

    # Horizontal rules
    html = re.sub(r'^---+$', '<hr>', html, flags=re.MULTILINE)

    # Tables
    def convert_table(match):
        lines = match.group(0).strip().split('\n')
        if len(lines) < 2:
            return match.group(0)

        result = ['<table>']

        # Header row
        headers = [cell.strip() for cell in lines[0].split('|') if cell.strip()]
        result.append('<thead><tr>')
        for h in headers:
            result.append(f'<th>{h}</th>')
        result.append('</tr></thead>')

        # Body rows (skip separator line)
        result.append('<tbody>')
        for line in lines[2:]:
            if line.strip():
                cells = [cell.strip() for cell in line.split('|') if cell.strip()]
                result.append('<tr>')
                for c in cells:
                    result.append(f'<td>{c}</td>')
                result.append('</tr>')
        result.append('</tbody>')
        result.append('</table>')

        return '\n'.join(result)

    # Match tables (lines starting with |)
    html = re.sub(r'(?:^\|.+\|$\n)+', convert_table, html, flags=re.MULTILINE)

    # Lists
    def convert_list(match):
        items = match.group(0).strip().split('\n')
        result = ['<ul>']
        for item in items:
            item = re.sub(r'^[\s]*[-*]\s+', '', item)
            if item:
                result.append(f'<li>{item}</li>')
        result.append('</ul>')
        return '\n'.join(result)

    html = re.sub(r'(?:^[\s]*[-*]\s+.+$\n?)+', convert_list, html, flags=re.MULTILINE)

    # Numbered lists
    def convert_ordered_list(match):
        items = match.group(0).strip().split('\n')
        result = ['<ol>']
        for item in items:
            item = re.sub(r'^[\s]*\d+\.\s+', '', item)
            if item:
                result.append(f'<li>{item}</li>')
        result.append('</ol>')
        return '\n'.join(result)

    html = re.sub(r'(?:^[\s]*\d+\.\s+.+$\n?)+', convert_ordered_list, html, flags=re.MULTILINE)

    # Restore code blocks
    for i, block in enumerate(code_blocks):
        lang_match = re.match(r'```(\w*)\n', block)
        lang = lang_match.group(1) if lang_match else ''
        code = re.sub(r'```\w*\n', '', block)
        code = re.sub(r'```$', '', code)
        # Escape HTML in code
        code = code.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')
        html = html.replace(f'__CODE_BLOCK_{i}__',
                           f'<pre><code class="language-{lang}">{code}</code></pre>')

    # Restore inline code
    for i, code in enumerate(inline_codes):
        code = code.replace('&', '&amp;').replace('<', '&lt;').replace('>', '&gt;')
        html = html.replace(f'__INLINE_CODE_{i}__', f'<code>{code}</code>')

    # Paragraphs (wrap lines not in tags)
    lines = html.split('\n')
    result = []
    in_block = False
    para_buffer = []

    for line in lines:
        stripped = line.strip()

        # Check if we're entering or in a block element
        if stripped.startswith('<pre') or stripped.startswith('<table') or stripped.startswith('<ul') or stripped.startswith('<ol'):
            in_block = True
        if stripped.endswith('</pre>') or stripped.endswith('</table>') or stripped.endswith('</ul>') or stripped.endswith('</ol>'):
            in_block = False
            result.append(line)
            continue

        if in_block or stripped.startswith('<') or not stripped:
            # Flush paragraph buffer
            if para_buffer:
                result.append('<p>' + ' '.join(para_buffer) + '</p>')
                para_buffer = []
            result.append(line)
        else:
            para_buffer.append(stripped)

    # Flush remaining
    if para_buffer:
        result.append('<p>' + ' '.join(para_buffer) + '</p>')

    return '\n'.join(result)


def convert_markdown_library(md_text: str) -> str:
    """Convert markdown to HTML using the markdown library."""
    md = markdown.Markdown(extensions=[
        'fenced_code',
        'tables',
        'toc',
        'nl2br',
    ])
    return md.convert(md_text)


HTML_TEMPLATE = '''<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>Running moq-rs Relay</title>

    <!-- Google Fonts - Roboto (Google's signature font) -->
    <link rel="preconnect" href="https://fonts.googleapis.com">
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
    <link href="https://fonts.googleapis.com/css2?family=Roboto:wght@300;400;500;700&family=Roboto+Mono:wght@400;500&display=swap" rel="stylesheet">

    <style>
        /* Google Material Design Color Palette */
        :root {
            /* Light theme (default) */
            --bg-primary: #ffffff;
            --bg-secondary: #f8f9fa;
            --bg-tertiary: #e8eaed;
            --bg-card: #ffffff;
            --bg-code: #f8f9fa;
            --bg-code-inline: #f1f3f4;

            --text-primary: #202124;
            --text-secondary: #5f6368;
            --text-muted: #9aa0a6;
            --text-code: #37474f;

            /* Google Blue */
            --accent-primary: #1a73e8;
            --accent-primary-hover: #1557b0;
            /* Google Green */
            --accent-success: #1e8e3e;
            /* Google Red */
            --accent-error: #d93025;
            /* Google Yellow */
            --accent-warning: #f9ab00;

            --border-color: #dadce0;
            --border-focus: #1a73e8;
            --shadow-sm: 0 1px 2px 0 rgba(60, 64, 67, 0.3), 0 1px 3px 1px rgba(60, 64, 67, 0.15);
            --shadow-md: 0 1px 3px 0 rgba(60, 64, 67, 0.3), 0 4px 8px 3px rgba(60, 64, 67, 0.15);
            --shadow-lg: 0 1px 3px 0 rgba(60, 64, 67, 0.3), 0 8px 16px 6px rgba(60, 64, 67, 0.15);

            --content-width: 900px;
            --radius: 8px;
        }

        /* Dark theme */
        @media (prefers-color-scheme: dark) {
            :root {
                --bg-primary: #202124;
                --bg-secondary: #292a2d;
                --bg-tertiary: #3c4043;
                --bg-card: #292a2d;
                --bg-code: #292a2d;
                --bg-code-inline: #3c4043;
                --text-primary: #e8eaed;
                --text-secondary: #9aa0a6;
                --text-muted: #5f6368;
                --text-code: #e8eaed;
                --accent-primary: #8ab4f8;
                --accent-primary-hover: #aecbfa;
                --accent-success: #81c995;
                --accent-error: #f28b82;
                --accent-warning: #fdd663;
                --border-color: #5f6368;
                --shadow-sm: 0 1px 2px 0 rgba(0, 0, 0, 0.3), 0 1px 3px 1px rgba(0, 0, 0, 0.15);
                --shadow-md: 0 1px 3px 0 rgba(0, 0, 0, 0.3), 0 4px 8px 3px rgba(0, 0, 0, 0.15);
                --shadow-lg: 0 1px 3px 0 rgba(0, 0, 0, 0.3), 0 8px 16px 6px rgba(0, 0, 0, 0.15);
            }
        }

        * {
            margin: 0;
            padding: 0;
            box-sizing: border-box;
        }

        html {
            scroll-behavior: smooth;
            font-size: 16px;
        }

        body {
            font-family: 'Roboto', -apple-system, BlinkMacSystemFont, 'Segoe UI', Helvetica, Arial, sans-serif;
            background: var(--bg-primary);
            color: var(--text-primary);
            line-height: 1.6;
            -webkit-font-smoothing: antialiased;
        }

        .container {
            max-width: var(--content-width);
            margin: 0 auto;
            padding: 2rem 1.5rem 4rem;
        }

        /* Typography - Google Style */
        h1 {
            font-size: 2.25rem;
            font-weight: 400;
            margin-bottom: 1rem;
            color: var(--text-primary);
            letter-spacing: -0.5px;
        }

        h2 {
            font-size: 1.5rem;
            font-weight: 500;
            margin-top: 3rem;
            margin-bottom: 1rem;
            padding-bottom: 0.5rem;
            border-bottom: 1px solid var(--border-color);
            color: var(--text-primary);
        }

        h3 {
            font-size: 1.125rem;
            font-weight: 500;
            margin-top: 2rem;
            margin-bottom: 0.75rem;
            color: var(--text-primary);
        }

        h4 {
            font-size: 1rem;
            font-weight: 500;
            color: var(--accent-primary);
            margin-top: 1.5rem;
            margin-bottom: 0.5rem;
        }

        p {
            margin-bottom: 1rem;
            color: var(--text-secondary);
        }

        strong {
            font-weight: 500;
            color: var(--text-primary);
        }

        em {
            font-style: italic;
        }

        a {
            color: var(--accent-primary);
            text-decoration: none;
            transition: color 0.2s ease;
        }

        a:hover {
            color: var(--accent-primary-hover);
            text-decoration: underline;
        }

        /* Lists */
        ul, ol {
            margin-bottom: 1rem;
            padding-left: 1.5rem;
            color: var(--text-secondary);
        }

        li {
            margin-bottom: 0.375rem;
        }

        li::marker {
            color: var(--accent-primary);
        }

        /* Code - Google Style */
        pre {
            background: var(--bg-code);
            border-radius: var(--radius);
            padding: 1rem 1.25rem;
            margin: 1rem 0;
            overflow-x: auto;
            border: 1px solid var(--border-color);
            position: relative;
        }

        pre code {
            font-family: 'Roboto Mono', 'Menlo', 'Monaco', 'Courier New', monospace;
            font-size: 0.875rem;
            line-height: 1.5;
            color: var(--text-code);
            background: none;
            padding: 0;
            border-radius: 0;
            border: none;
        }

        code {
            font-family: 'Roboto Mono', 'Menlo', 'Monaco', 'Courier New', monospace;
            font-size: 0.875em;
            background: var(--bg-code-inline);
            color: var(--accent-error);
            padding: 0.125em 0.375em;
            border-radius: 4px;
        }

        /* Tables - Google Style */
        table {
            width: 100%;
            border-collapse: collapse;
            margin: 1rem 0;
            background: var(--bg-card);
            border-radius: var(--radius);
            overflow: hidden;
            box-shadow: var(--shadow-sm);
        }

        th {
            background: var(--bg-tertiary);
            color: var(--text-primary);
            font-weight: 500;
            text-align: left;
            padding: 0.75rem 1rem;
            font-size: 0.875rem;
        }

        td {
            padding: 0.75rem 1rem;
            border-top: 1px solid var(--border-color);
            color: var(--text-secondary);
            font-size: 0.875rem;
        }

        tr:hover td {
            background: var(--bg-secondary);
        }

        /* Blockquotes */
        blockquote {
            border-left: 3px solid var(--accent-primary);
            background: var(--bg-secondary);
            padding: 0.75rem 1rem;
            margin: 1rem 0;
            border-radius: 0 var(--radius) var(--radius) 0;
            color: var(--text-secondary);
        }

        hr {
            border: none;
            height: 1px;
            background: var(--border-color);
            margin: 2rem 0;
        }

        /* Note/Warning boxes */
        .note {
            background: #e8f0fe;
            border-left: 4px solid var(--accent-primary);
            padding: 1rem;
            margin: 1rem 0;
            border-radius: 0 var(--radius) var(--radius) 0;
        }

        @media (prefers-color-scheme: dark) {
            .note {
                background: rgba(138, 180, 248, 0.1);
            }
        }

        /* Scrollbar - Google Style */
        ::-webkit-scrollbar {
            width: 8px;
            height: 8px;
        }

        ::-webkit-scrollbar-track {
            background: transparent;
        }

        ::-webkit-scrollbar-thumb {
            background: var(--border-color);
            border-radius: 4px;
        }

        ::-webkit-scrollbar-thumb:hover {
            background: var(--text-muted);
        }

        /* Responsive */
        @media (max-width: 768px) {
            .container {
                padding: 1.5rem 1rem 3rem;
            }

            h1 {
                font-size: 1.75rem;
            }

            h2 {
                font-size: 1.25rem;
            }

            pre {
                padding: 0.75rem 1rem;
                font-size: 0.8125rem;
            }
        }

        /* Print */
        @media print {
            body {
                background: white;
                color: black;
            }

            pre {
                background: #f5f5f5;
                border: 1px solid #ddd;
            }

            a {
                color: #1a73e8;
            }
        }

        :target {
            scroll-margin-top: 1.5rem;
        }

        .footer {
            margin-top: 3rem;
            padding-top: 1.5rem;
            border-top: 1px solid var(--border-color);
            text-align: center;
            color: var(--text-muted);
            font-size: 0.875rem;
        }

        /* Copy button for code blocks */
        .copy-btn {
            position: absolute;
            top: 0.5rem;
            right: 0.5rem;
            padding: 0.25rem 0.5rem;
            font-size: 0.75rem;
            font-family: 'Roboto', sans-serif;
            font-weight: 500;
            background: var(--bg-tertiary);
            color: var(--text-secondary);
            border: none;
            border-radius: 4px;
            cursor: pointer;
            opacity: 0;
            transition: opacity 0.2s, background 0.2s;
        }

        pre:hover .copy-btn {
            opacity: 1;
        }

        .copy-btn:hover {
            background: var(--border-color);
            color: var(--text-primary);
        }

        /* Header styling */
        .header {
            margin-bottom: 2rem;
        }

        .header p {
            color: var(--text-muted);
            font-size: 1rem;
        }
    </style>
</head>
<body>
    <div class="container">
        {content}

        <div class="footer">
            <p>Generated for moq-rs</p>
        </div>
    </div>

    <script>
        // Add copy button to code blocks
        document.querySelectorAll('pre').forEach(block => {
            const button = document.createElement('button');
            button.className = 'copy-btn';
            button.textContent = 'Copy';
            block.appendChild(button);

            button.addEventListener('click', async () => {
                const code = block.querySelector('code');
                await navigator.clipboard.writeText(code.textContent);
                button.textContent = 'Copied!';
                setTimeout(() => button.textContent = 'Copy', 2000);
            });
        });

        // Smooth scrolling for anchor links
        document.querySelectorAll('a[href^="#"]').forEach(anchor => {
            anchor.addEventListener('click', function (e) {
                const href = this.getAttribute('href');
                const target = document.querySelector(href);
                if (target) {
                    e.preventDefault();
                    target.scrollIntoView({
                        behavior: 'smooth',
                        block: 'start'
                    });
                    // Update URL hash
                    history.pushState(null, null, href);
                }
                // If target not found, let default anchor behavior work
            });
        });
    </script>
</body>
</html>
'''


def main():
    input_file = sys.argv[1] if len(sys.argv) > 1 else 'run.md'
    output_file = sys.argv[2] if len(sys.argv) > 2 else 'run.html'

    input_path = Path(input_file)
    output_path = Path(output_file)

    if not input_path.exists():
        print(f"Error: Input file '{input_file}' not found.")
        sys.exit(1)

    print(f"Converting {input_file} to {output_file}...")

    md_text = input_path.read_text(encoding='utf-8')

    if HAS_MARKDOWN:
        print("Using 'markdown' library for conversion...")
        html_content = convert_markdown_library(md_text)
    else:
        print("Using built-in converter...")
        html_content = convert_markdown_builtin(md_text)

    final_html = HTML_TEMPLATE.replace('{content}', html_content)

    output_path.write_text(final_html, encoding='utf-8')

    print()
    print("=" * 50)
    print("  HTML generated successfully!")
    print("=" * 50)
    print()
    print(f"  Input:  {input_file}")
    print(f"  Output: {output_file}")
    print()
    print("  Open in browser:")
    print(f"    open {output_file}")
    print()
    print("  Features:")
    print("    - Roboto font (Google's signature font)")
    print("    - Material Design color palette")
    print("    - Dark/Light theme support")
    print("    - Responsive design")
    print("    - Copy button on code blocks")
    print()


if __name__ == '__main__':
    main()
