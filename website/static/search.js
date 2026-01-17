(function() {
    function initSearch() {
        const searchInput = document.getElementById('spec-search-input');
        const searchResults = document.getElementById('spec-search-results');
        let searchIndex = null;
        
        if (!searchInput || !searchResults) return;
        
        const shortcutMod = document.querySelector('.spec-search-shortcut-mod');
        if (shortcutMod) {
            shortcutMod.textContent = navigator.platform.indexOf('Mac') > -1 ? '⌘' : 'Ctrl+';
        }
        
        fetch('/search_index.en.json')
            .then(response => response.json())
            .then(data => {
                searchIndex = elasticlunr.Index.load(data);
            })
            .catch(err => {
                console.error('Failed to load search index:', err);
            });
        
        function search(query) {
            if (!searchIndex || !query || query.length < 2) {
                return [];
            }
            
            const results = searchIndex.search(query, {
                fields: {
                    title: { boost: 3 },
                    body: { boost: 1 }
                },
                bool: "OR",
                expand: true
            });
            
            return results.slice(0, 10).map(result => {
                const doc = searchIndex.documentStore.getDoc(result.ref);
                return {
                    title: doc.title,
                    path: result.ref,
                    body: doc.body ? doc.body.substring(0, 150) + '...' : ''
                };
            });
        }
        
        function escapeHtml(text) {
            const div = document.createElement('div');
            div.textContent = text;
            return div.innerHTML;
        }
        
        function renderResults(results) {
            if (results.length === 0) {
                searchResults.innerHTML = '<div class="search-no-results">No results found</div>';
                searchResults.style.display = 'block';
                return;
            }
            
            searchResults.innerHTML = results.map(result =>
                '<a href="' + result.path + '" class="search-result-item">' +
                '<div class="search-result-title">' + escapeHtml(result.title) + '</div>' +
                '<div class="search-result-body">' + escapeHtml(result.body) + '</div>' +
                '</a>'
            ).join('');
            searchResults.style.display = 'block';
        }
        
        let debounceTimer;
        searchInput.addEventListener('input', e => {
            clearTimeout(debounceTimer);
            const query = e.target.value.trim();
            
            if (query.length < 2) {
                searchResults.style.display = 'none';
                return;
            }
            
            debounceTimer = setTimeout(() => {
                renderResults(search(query));
            }, 150);
        });
        
        searchInput.addEventListener('focus', () => {
            if (searchInput.value.trim().length >= 2) {
                searchResults.style.display = 'block';
            }
        });
        
        document.addEventListener('click', e => {
            if (!searchInput.contains(e.target) && !searchResults.contains(e.target)) {
                searchResults.style.display = 'none';
            }
        });
        
        searchInput.addEventListener('keydown', e => {
            if (e.key === 'Escape') {
                searchResults.style.display = 'none';
                searchInput.blur();
            }
        });
        
        document.addEventListener('keydown', e => {
            if ((e.metaKey || e.ctrlKey) && e.key === 'k') {
                e.preventDefault();
                searchInput.focus();
                searchInput.select();
            }
        });
    }
    
    if (document.readyState === 'loading') {
        document.addEventListener('DOMContentLoaded', initSearch);
    } else {
        initSearch();
    }
})();
