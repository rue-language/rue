// Add "Back to Rue" link in the header
(function() {
    const rightButtons = document.querySelector('.right-buttons');
    if (rightButtons) {
        const link = document.createElement('a');
        link.href = '/';
        link.className = 'back-to-site';
        link.title = 'Back to Rue website';
        link.setAttribute('aria-label', 'Back to Rue website');
        link.innerHTML = '← Rue';
        rightButtons.insertBefore(link, rightButtons.firstChild);
    }
})();
