#!/usr/bin/env bash
#
# generate-design-html.sh
# Converts DESIGN.md to a beautifully styled HTML document
#
# Usage: ./generate-design-html.sh [input.md] [output.html]
#
# Dependencies:
#   - pandoc (for markdown conversion)
#   OR
#   - Basic bash (fallback using embedded converter)
#

set -euo pipefail

INPUT="${1:-DESIGN.md}"
OUTPUT="${2:-DESIGN.html}"

# Modern color palette - Deep ocean theme with warm accents
cat > "$OUTPUT" << 'HTMLHEADER'
<!DOCTYPE html>
<html lang="en">
<head>
    <meta charset="UTF-8">
    <meta name="viewport" content="width=device-width, initial-scale=1.0">
    <title>MoQ-RS Design Document</title>

    <!-- Inter Font - Modern, highly readable -->
    <link rel="preconnect" href="https://fonts.googleapis.com">
    <link rel="preconnect" href="https://fonts.gstatic.com" crossorigin>
    <link href="https://fonts.googleapis.com/css2?family=Inter:wght@300;400;500;600;700&family=JetBrains+Mono:wght@400;500&display=swap" rel="stylesheet">

    <style>
        /* ═══════════════════════════════════════════════════════════════
           Color Palette - Deep Ocean with Warm Accents
           ═══════════════════════════════════════════════════════════════ */
        :root {
            /* Background colors - Neutral dark palette */
            --bg-primary: #1a1a1a;      /* Charcoal */
            --bg-secondary: #262626;    /* Dark gray */
            --bg-tertiary: #404040;     /* Medium gray */
            --bg-card: #262626;
            --bg-code: #1f1f1f;
            --bg-code-inline: #333333;

            /* Text colors */
            --text-primary: #f5f5f5;    /* Off-white */
            --text-secondary: #a3a3a3;  /* Muted gray */
            --text-muted: #737373;      /* Subtle gray */
            --text-code: #e5e5e5;       /* Code text */

            /* Accent colors */
            --accent-primary: #60a5fa;  /* Soft blue */
            --accent-secondary: #a78bfa; /* Purple */
            --accent-tertiary: #4ade80; /* Green */
            --accent-warm: #fbbf24;     /* Amber */
            --accent-pink: #f472b6;     /* Pink */

            /* Syntax highlighting */
            --syntax-keyword: #f472b6;
            --syntax-string: #4ade80;
            --syntax-comment: #737373;
            --syntax-number: #fbbf24;
            --syntax-function: #60a5fa;
            --syntax-type: #a78bfa;

            /* Borders & shadows */
            --border-color: #404040;
            --border-accent: #60a5fa;
            --shadow-lg: 0 25px 50px -12px rgba(0, 0, 0, 0.5);
            --shadow-glow: 0 0 40px rgba(96, 165, 250, 0.15);

            /* Spacing */
            --content-width: 900px;
            --spacing-unit: 1rem;
        }

        /* Light theme override */
        @media (prefers-color-scheme: light) {
            :root {
                --bg-primary: #ffffff;
                --bg-secondary: #f8fafc;
                --bg-tertiary: #e2e8f0;
                --bg-card: #ffffff;
                --bg-code: #f8fafc;
                --bg-code-inline: #f1f5f9;

                --text-primary: #0f172a;
                --text-secondary: #475569;
                --text-muted: #94a3b8;
                --text-code: #1e293b;

                --border-color: #cbd5e1;
                --accent-warm: #c2410c;
                --shadow-glow: 0 0 40px rgba(96, 165, 250, 0.1);
            }
        }

        /* ═══════════════════════════════════════════════════════════════
           Base Styles
           ═══════════════════════════════════════════════════════════════ */
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
            font-family: 'Inter', -apple-system, BlinkMacSystemFont, 'Segoe UI', Roboto, sans-serif;
            background: var(--bg-primary);
            color: var(--text-primary);
            line-height: 1.7;
            -webkit-font-smoothing: antialiased;
            -moz-osx-font-smoothing: grayscale;
        }

        /* ═══════════════════════════════════════════════════════════════
           Layout
           ═══════════════════════════════════════════════════════════════ */
        .container {
            max-width: var(--content-width);
            margin: 0 auto;
            padding: 3rem 2rem 6rem;
        }

        /* ═══════════════════════════════════════════════════════════════
           Typography
           ═══════════════════════════════════════════════════════════════ */
        h1 {
            font-size: 2.75rem;
            font-weight: 700;
            color: var(--text-primary);
            margin-bottom: 0.5rem;
            letter-spacing: -0.03em;
            background: linear-gradient(135deg, var(--accent-primary), var(--accent-secondary));
            -webkit-background-clip: text;
            -webkit-text-fill-color: transparent;
            background-clip: text;
        }

        h2 {
            font-size: 1.875rem;
            font-weight: 600;
            color: var(--text-primary);
            margin-top: 4rem;
            margin-bottom: 1.5rem;
            padding-bottom: 0.75rem;
            border-bottom: 2px solid var(--border-color);
            letter-spacing: -0.02em;
        }

        h2::before {
            content: '';
            display: inline-block;
            width: 4px;
            height: 1.5rem;
            background: linear-gradient(180deg, var(--accent-primary), var(--accent-secondary));
            margin-right: 0.75rem;
            border-radius: 2px;
            vertical-align: middle;
        }

        h3 {
            font-size: 1.375rem;
            font-weight: 600;
            color: var(--text-primary);
            margin-top: 2.5rem;
            margin-bottom: 1rem;
            letter-spacing: -0.01em;
        }

        h4 {
            font-size: 1.125rem;
            font-weight: 600;
            color: var(--accent-primary);
            margin-top: 2rem;
            margin-bottom: 0.75rem;
        }

        p {
            margin-bottom: 1.25rem;
            color: var(--text-secondary);
        }

        strong {
            font-weight: 600;
            color: var(--text-primary);
        }

        em {
            font-style: italic;
            color: var(--accent-tertiary);
        }

        /* ═══════════════════════════════════════════════════════════════
           Links
           ═══════════════════════════════════════════════════════════════ */
        a {
            color: var(--accent-primary);
            text-decoration: none;
            transition: all 0.2s ease;
            border-bottom: 1px solid transparent;
        }

        a:hover {
            color: var(--accent-secondary);
            border-bottom-color: var(--accent-secondary);
        }

        /* ═══════════════════════════════════════════════════════════════
           Lists
           ═══════════════════════════════════════════════════════════════ */
        ul, ol {
            margin-bottom: 1.5rem;
            padding-left: 1.5rem;
            color: var(--text-secondary);
        }

        li {
            margin-bottom: 0.5rem;
            padding-left: 0.5rem;
        }

        li::marker {
            color: var(--accent-primary);
        }

        /* ═══════════════════════════════════════════════════════════════
           Code Blocks
           ═══════════════════════════════════════════════════════════════ */
        pre {
            background: var(--bg-code);
            border-radius: 12px;
            padding: 1.5rem;
            margin: 1.5rem 0;
            overflow-x: auto;
            border: 1px solid var(--border-color);
            box-shadow: var(--shadow-lg);
        }

        pre code {
            font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Consolas, monospace;
            font-size: 0.875rem;
            line-height: 1.6;
            color: var(--text-code);
            background: none;
            padding: 0;
            border-radius: 0;
        }

        code {
            font-family: 'JetBrains Mono', 'Fira Code', 'SF Mono', Consolas, monospace;
            font-size: 0.875em;
            background: var(--bg-code-inline);
            color: var(--accent-warm);
            padding: 0.2em 0.5em;
            border-radius: 6px;
            border: 1px solid var(--border-color);
        }

        /* ═══════════════════════════════════════════════════════════════
           Tables
           ═══════════════════════════════════════════════════════════════ */
        table {
            width: 100%;
            border-collapse: collapse;
            margin: 1.5rem 0;
            background: var(--bg-card);
            border-radius: 12px;
            overflow: hidden;
            box-shadow: var(--shadow-lg);
        }

        th {
            background: var(--bg-tertiary);
            color: var(--text-primary);
            font-weight: 600;
            text-align: left;
            padding: 1rem 1.25rem;
            font-size: 0.875rem;
            text-transform: uppercase;
            letter-spacing: 0.05em;
        }

        td {
            padding: 1rem 1.25rem;
            border-top: 1px solid var(--border-color);
            color: var(--text-secondary);
        }

        tr:hover td {
            background: var(--bg-secondary);
        }

        /* ═══════════════════════════════════════════════════════════════
           Blockquotes
           ═══════════════════════════════════════════════════════════════ */
        blockquote {
            border-left: 4px solid var(--accent-primary);
            background: var(--bg-secondary);
            padding: 1rem 1.5rem;
            margin: 1.5rem 0;
            border-radius: 0 12px 12px 0;
            color: var(--text-secondary);
        }

        blockquote p:last-child {
            margin-bottom: 0;
        }

        /* ═══════════════════════════════════════════════════════════════
           Horizontal Rules
           ═══════════════════════════════════════════════════════════════ */
        hr {
            border: none;
            height: 1px;
            background: linear-gradient(90deg, transparent, var(--border-color), transparent);
            margin: 3rem 0;
        }

        /* ═══════════════════════════════════════════════════════════════
           Table of Contents
           ═══════════════════════════════════════════════════════════════ */
        .toc {
            background: var(--bg-card);
            border: 1px solid var(--border-color);
            border-radius: 16px;
            padding: 2rem;
            margin: 2rem 0 3rem;
            box-shadow: var(--shadow-glow);
        }

        .toc h2 {
            margin-top: 0;
            margin-bottom: 1.5rem;
            font-size: 1.25rem;
            border-bottom: none;
            padding-bottom: 0;
        }

        .toc h2::before {
            display: none;
        }

        .toc ol {
            list-style: none;
            padding-left: 0;
            margin-bottom: 0;
            display: grid;
            grid-template-columns: repeat(auto-fit, minmax(280px, 1fr));
            gap: 0.5rem 2rem;
        }

        .toc li {
            padding-left: 0;
            margin-bottom: 0;
        }

        .toc a {
            display: flex;
            align-items: center;
            padding: 0.5rem 0;
            color: var(--text-secondary);
            transition: all 0.2s ease;
        }

        .toc a:hover {
            color: var(--accent-primary);
            transform: translateX(4px);
        }

        .toc a::before {
            content: '';
            display: inline-block;
            width: 6px;
            height: 6px;
            background: var(--accent-primary);
            border-radius: 50%;
            margin-right: 0.75rem;
            opacity: 0.5;
            transition: opacity 0.2s ease;
        }

        .toc a:hover::before {
            opacity: 1;
        }

        /* ═══════════════════════════════════════════════════════════════
           ASCII Diagrams - Special styling for code blocks with diagrams
           ═══════════════════════════════════════════════════════════════ */
        pre:has(code) {
            position: relative;
        }

        /* Style for diagram-like code blocks */
        pre code {
            display: block;
            white-space: pre;
        }

        /* ═══════════════════════════════════════════════════════════════
           Header Section
           ═══════════════════════════════════════════════════════════════ */
        .header-meta {
            display: flex;
            flex-wrap: wrap;
            gap: 1.5rem;
            margin-bottom: 2rem;
            padding-bottom: 2rem;
            border-bottom: 1px solid var(--border-color);
        }

        .meta-item {
            display: flex;
            align-items: center;
            gap: 0.5rem;
            font-size: 0.875rem;
            color: var(--text-muted);
        }

        .meta-item strong {
            color: var(--text-secondary);
        }

        /* ═══════════════════════════════════════════════════════════════
           Feature Box
           ═══════════════════════════════════════════════════════════════ */
        .feature-box {
            background: linear-gradient(135deg, var(--bg-secondary), var(--bg-tertiary));
            border: 1px solid var(--border-color);
            border-radius: 16px;
            padding: 2rem;
            margin: 2rem 0;
        }

        /* ═══════════════════════════════════════════════════════════════
           Scrollbar Styling
           ═══════════════════════════════════════════════════════════════ */
        ::-webkit-scrollbar {
            width: 8px;
            height: 8px;
        }

        ::-webkit-scrollbar-track {
            background: var(--bg-secondary);
            border-radius: 4px;
        }

        ::-webkit-scrollbar-thumb {
            background: var(--border-color);
            border-radius: 4px;
        }

        ::-webkit-scrollbar-thumb:hover {
            background: var(--text-muted);
        }

        /* ═══════════════════════════════════════════════════════════════
           Responsive Design
           ═══════════════════════════════════════════════════════════════ */
        @media (max-width: 768px) {
            .container {
                padding: 2rem 1rem 4rem;
            }

            h1 {
                font-size: 2rem;
            }

            h2 {
                font-size: 1.5rem;
            }

            pre {
                padding: 1rem;
                border-radius: 8px;
                font-size: 0.8rem;
            }

            table {
                font-size: 0.875rem;
            }

            th, td {
                padding: 0.75rem;
            }

            .toc ol {
                grid-template-columns: 1fr;
            }
        }

        /* ═══════════════════════════════════════════════════════════════
           Print Styles
           ═══════════════════════════════════════════════════════════════ */
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
                color: #0066cc;
            }

            h1 {
                background: none;
                -webkit-text-fill-color: inherit;
                color: black;
            }
        }

        /* ═══════════════════════════════════════════════════════════════
           Animation & Transitions
           ═══════════════════════════════════════════════════════════════ */
        h2, h3, h4 {
            transition: color 0.2s ease;
        }

        h2:hover, h3:hover {
            color: var(--accent-primary);
        }

        /* Smooth anchor scrolling offset */
        :target {
            scroll-margin-top: 2rem;
        }

        /* ═══════════════════════════════════════════════════════════════
           Footer
           ═══════════════════════════════════════════════════════════════ */
        .footer {
            margin-top: 4rem;
            padding-top: 2rem;
            border-top: 1px solid var(--border-color);
            text-align: center;
            color: var(--text-muted);
            font-size: 0.875rem;
        }

        .footer em {
            color: var(--text-secondary);
        }
    </style>
</head>
<body>
    <div class="container">
HTMLHEADER

# Check if pandoc is available
if command -v pandoc &> /dev/null; then
    echo "Using pandoc for conversion..."

    # Convert markdown to HTML body using pandoc
    pandoc "$INPUT" \
        --from markdown \
        --to html5 \
        --no-highlight \
        --wrap=none \
        >> "$OUTPUT"
else
    echo "Pandoc not found. Using basic bash conversion..."

    # Basic markdown to HTML conversion
    # This handles the most common markdown elements

    sed -E '
        # Escape HTML special chars first (careful with order)
        # s/&/\&amp;/g
        # s/</\&lt;/g
        # s/>/\&gt;/g

        # Headers
        s/^# (.*)$/<h1>\1<\/h1>/
        s/^## (.*)$/<h2 id="\L\1">\E\1<\/h2>/
        s/^### (.*)$/<h3>\1<\/h3>/
        s/^#### (.*)$/<h4>\1<\/h4>/

        # Bold and italic
        s/\*\*([^*]+)\*\*/<strong>\1<\/strong>/g
        s/\*([^*]+)\*/<em>\1<\/em>/g

        # Inline code (backticks)
        s/`([^`]+)`/<code>\1<\/code>/g

        # Links
        s/\[([^\]]+)\]\(([^)]+)\)/<a href="\2">\1<\/a>/g

        # Horizontal rules
        s/^---$/<hr>/
        s/^___$/<hr>/

        # List items (basic)
        s/^- (.*)$/<li>\1<\/li>/
        s/^\* (.*)$/<li>\1<\/li>/
        s/^[0-9]+\. (.*)$/<li>\1<\/li>/

        # Blockquotes
        s/^> (.*)$/<blockquote>\1<\/blockquote>/

    ' "$INPUT" | \

    # Handle code blocks (triple backticks)
    awk '
        BEGIN { in_code = 0 }
        /^```/ {
            if (in_code) {
                print "</code></pre>"
                in_code = 0
            } else {
                lang = substr($0, 4)
                print "<pre><code class=\"language-" lang "\">"
                in_code = 1
            }
            next
        }
        { print }
    ' | \

    # Wrap paragraphs (lines that are not already wrapped in tags)
    awk '
        {
            if ($0 ~ /^</ || $0 ~ /^$/ || $0 ~ /^\|/) {
                print
            } else {
                print "<p>" $0 "</p>"
            }
        }
    ' >> "$OUTPUT"
fi

# Close HTML
cat >> "$OUTPUT" << 'HTMLFOOTER'
        <div class="footer">
            <em>Document generated for moq-rs v0.12.2</em>
            <br>
            Generated on <script>document.write(new Date().toLocaleDateString())</script>
        </div>
    </div>

    <script>
        // Add smooth scrolling for anchor links
        document.querySelectorAll('a[href^="#"]').forEach(anchor => {
            anchor.addEventListener('click', function (e) {
                e.preventDefault();
                const target = document.querySelector(this.getAttribute('href'));
                if (target) {
                    target.scrollIntoView({
                        behavior: 'smooth',
                        block: 'start'
                    });
                }
            });
        });

        // Add copy button to code blocks
        document.querySelectorAll('pre').forEach(block => {
            const button = document.createElement('button');
            button.textContent = 'Copy';
            button.style.cssText = `
                position: absolute;
                top: 0.5rem;
                right: 0.5rem;
                padding: 0.25rem 0.75rem;
                font-size: 0.75rem;
                background: var(--bg-tertiary);
                color: var(--text-secondary);
                border: 1px solid var(--border-color);
                border-radius: 6px;
                cursor: pointer;
                opacity: 0;
                transition: opacity 0.2s;
            `;

            block.style.position = 'relative';
            block.appendChild(button);

            block.addEventListener('mouseenter', () => button.style.opacity = '1');
            block.addEventListener('mouseleave', () => button.style.opacity = '0');

            button.addEventListener('click', async () => {
                const code = block.querySelector('code');
                await navigator.clipboard.writeText(code.textContent);
                button.textContent = 'Copied!';
                setTimeout(() => button.textContent = 'Copy', 2000);
            });
        });
    </script>
</body>
</html>
HTMLFOOTER

echo ""
echo "============================================"
echo "  HTML generated successfully!"
echo "============================================"
echo ""
echo "  Input:  $INPUT"
echo "  Output: $OUTPUT"
echo ""
echo "  Open in browser:"
echo "    open $OUTPUT"
echo ""
echo "  Features:"
echo "    - Inter font (modern, readable)"
echo "    - JetBrains Mono for code"
echo "    - Dark theme (light theme via prefers-color-scheme)"
echo "    - Responsive design"
echo "    - Copy button on code blocks"
echo "    - Smooth anchor scrolling"
echo ""
