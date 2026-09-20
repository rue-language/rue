(function() {
    function initSearch() {
        const input = document.getElementById('spec-search-input');
        const results = document.getElementById('spec-search-results');
        if (!input || !results) return;

        const shortcut = document.querySelector('.spec-search-shortcut-mod');
        if (shortcut) shortcut.textContent = navigator.platform.includes('Mac') ? '⌘' : 'Ctrl+';

        let index;
        let loading;
        let request = 0;
        let active = -1;
        let timer;

        function loadIndex() {
            if (index) return Promise.resolve(index);
            if (loading) return loading;
            loading = fetch('/search_index.en.json')
                .then(response => {
                    if (!response.ok) throw new Error('Search download failed');
                    return response.json();
                })
                .then(data => {
                    if (data.format_version !== 2 || !Array.isArray(data.documents)) {
                        throw new Error('Unsupported Gazette search documents');
                    }
                    // Gazette supplies text and heading boundaries. Elasticlunr
                    // remains the single owner of tokenization and query ranking.
                    const built = elasticlunr(function() {
                        this.setRef('ref');
                        ['title', 'heading', 'description', 'body'].forEach(field => this.addField(field));
                    });
                    data.documents.forEach(doc => built.addDoc(doc));
                    index = built;
                    return built;
                })
                .catch(error => { loading = null; throw error; });
            return loading;
        }

        function hide() {
            results.style.display = 'none';
            input.setAttribute('aria-expanded', 'false');
            input.removeAttribute('aria-activedescendant');
            active = -1;
        }

        function show() {
            results.style.display = 'block';
            input.setAttribute('aria-expanded', 'true');
            active = -1;
            input.removeAttribute('aria-activedescendant');
        }

        function message(text) {
            const status = document.createElement('div');
            status.className = 'search-no-results';
            status.setAttribute('role', 'status');
            status.textContent = text;
            results.replaceChildren(status);
            show();
        }

        function snippet(body, query) {
            const text = (body || '').replace(/\s+/g, ' ').trim();
            const terms = query.toLowerCase().split(/\s+/).filter(term => term.length >= 2);
            const hits = terms.map(term => text.toLowerCase().indexOf(term)).filter(at => at >= 0);
            const start = hits.length ? Math.max(0, Math.min(...hits) - 55) : 0;
            return (start ? '…' : '') + text.slice(start, start + 180) + (start + 180 < text.length ? '…' : '');
        }

        function render(query) {
            const matches = index.search(query, {
                fields: {title: {boost: 3}, heading: {boost: 5}, description: {boost: 2}, body: {boost: 1}},
                bool: 'OR', expand: true,
            });
            // Show a page once at its highest-ranked passage. Heading matches
            // link directly to the section; introductions retain the page URL.
            const pages = new Map();
            matches.forEach(match => {
                const doc = index.documentStore.getDoc(match.ref);
                if (!pages.has(doc.page_ref)) pages.set(doc.page_ref, doc);
            });
            results.replaceChildren();
            Array.from(pages.values()).slice(0, 10).forEach((doc, number) => {
                const url = new URL(doc.ref, location.href);
                if (!['https:', 'http:'].includes(url.protocol)) return;
                const link = document.createElement('a');
                link.href = url.href;
                link.className = 'search-result-item';
                link.id = 'search-result-' + number;
                link.setAttribute('role', 'option');
                link.setAttribute('aria-selected', 'false');
                if (doc.heading) {
                    const page = document.createElement('div');
                    page.className = 'search-result-page';
                    page.textContent = doc.title;
                    link.appendChild(page);
                }
                const title = document.createElement('div');
                title.className = 'search-result-title';
                title.textContent = doc.heading || doc.title;
                const body = document.createElement('div');
                body.className = 'search-result-body';
                body.textContent = snippet(doc.body, query);
                link.append(title, body);
                results.appendChild(link);
            });
            if (!results.children.length) message('No results found');
            else show();
        }

        async function runSearch() {
            const query = input.value.trim();
            const current = ++request;
            if (query.length < 2) { hide(); return; }
            if (!index) message('Loading search…');
            try {
                await loadIndex();
                if (current === request) render(query);
            } catch (_) {
                if (current === request) message('Search is unavailable. Type again to retry.');
            }
        }

        input.addEventListener('input', () => {
            clearTimeout(timer);
            request += 1;
            if (input.value.trim().length < 2) hide();
            else timer = setTimeout(runSearch, 150);
        });
        input.addEventListener('focus', () => {
            if (input.value.trim().length >= 2) runSearch();
            else loadIndex().catch(() => {});
        });
        document.addEventListener('click', event => {
            if (!input.contains(event.target) && !results.contains(event.target)) {
                request += 1;
                clearTimeout(timer);
                hide();
            }
        });
        input.addEventListener('keydown', event => {
            if (event.key === 'Escape') {
                request += 1;
                clearTimeout(timer);
                hide();
                input.blur();
                return;
            }
            const links = Array.from(results.querySelectorAll('a'));
            if (!links.length || results.style.display === 'none') return;
            if (event.key === 'ArrowDown' || event.key === 'ArrowUp') {
                event.preventDefault();
                const step = event.key === 'ArrowDown' ? 1 : -1;
                active = active < 0 ? (step > 0 ? 0 : links.length - 1) : (active + step + links.length) % links.length;
                links.forEach((link, number) => link.setAttribute('aria-selected', String(number === active)));
                input.setAttribute('aria-activedescendant', links[active].id);
                links[active].scrollIntoView({block: 'nearest'});
            } else if (event.key === 'Enter' && active >= 0) {
                event.preventDefault();
                links[active].click();
            }
        });
        document.addEventListener('keydown', event => {
            if ((event.metaKey || event.ctrlKey) && event.key === 'k') {
                event.preventDefault();
                input.focus();
                input.select();
            }
        });
    }
    if (document.readyState === 'loading') document.addEventListener('DOMContentLoaded', initSearch);
    else initSearch();
})();
