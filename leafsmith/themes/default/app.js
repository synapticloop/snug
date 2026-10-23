const bar = document.querySelector('.leafsmith-topbar');
const barHeight = () => (bar ? Math.round(bar.getBoundingClientRect().height) : 72);
const publishBarHeight = () => {
  const height = barHeight();
  if (height) document.documentElement.style.setProperty('--leafsmith-bar-h', height + 'px');
  return height;
};

// The bar's real height is not knowable in CSS: 72px is its minimum, and a taller logo, a site
// title that wraps onto a second line, or a nav slot of its own makes it grow past that. Every
// offset in the stylesheet that positions something against the bar - the two sticky rails and
// the anchors' scroll-margin - is written against --leafsmith-bar-h, which is measured here
// rather than assumed. Offsets pinned to the minimum slide a sticky rail under a taller bar.
publishBarHeight();
// A resize rewraps the title and can change the height; a webfont or a late logo can change it
// after load. Either one would otherwise leave the rails offset by a stale amount.
addEventListener('resize', publishBarHeight, { passive: true });
addEventListener('load', publishBarHeight);
if (document.fonts && document.fonts.ready) document.fonts.ready.then(publishBarHeight);

// Marks the rail entry for the heading currently being read, so the right-hand
// TOC shows position within the chapter the way the left nav shows position
// within the book. Pages without headings leave the rail unmarked.
//
// Position is computed from scroll offset rather than observed with
// IntersectionObserver: a scrollbar drag or an End-key jump moves the page in a
// single frame, so an observer never sees the headings the viewport passed over
// and the rail would freeze on a stale entry.
const toc = document.querySelector('.leafsmith-toc');
if (toc) {
  // Only the entries the engine generated, not every anchor in the rail. A chrome fragment
  // placed at the bottom of this sidebar can hold links of its own - a "back to top" pointing
  // at the reading column resolves to a real element, so selecting on href alone would enroll
  // it as a heading and it would win the mark at the end of the page.
  const links = [...toc.querySelectorAll('a.leafsmith-toc-link')];
  // The engine writes each anchor as "#" + the heading id, so the href minus its
  // hash is that id verbatim; no decoding step is wanted. Entries whose heading
  // is missing are dropped so link and target stay aligned by index.
  const entries = links
    .map(link => ({ link, target: document.getElementById(link.getAttribute('href').slice(1)) }))
    .filter(entry => entry.target);

  if (entries.length) {
    let current = -1;
    const mark = index => {
      if (index === current) return;
      current = index;
      entries.forEach((entry, i) => {
        entry.link.classList.toggle('leafsmith-active', i === index);
        if (i === index) entry.link.setAttribute('aria-current', 'true');
        else entry.link.removeAttribute('aria-current');
      });
    };

    // The reading line sits just below the header. The active entry is the last
    // heading above it, so the rail stays steady through a long stretch of prose
    // between headings instead of going blank.
    //
    // The line is measured from the same bar the rails are offset against, so the mark and the
    // layout can never disagree about where the bar ends.
    const GAP = 19;
    let line = barHeight() + GAP;
    let queued = false;
    const update = () => {
      queued = false;
      let index = 0;
      for (let i = 0; i < entries.length; i++) {
        if (entries[i].target.getBoundingClientRect().top <= line) index = i;
        else break;
      }
      mark(index);
    };

    addEventListener('scroll', () => {
      if (queued) return;
      queued = true;
      requestAnimationFrame(update);
    }, { passive: true });
    // Re-measure before deciding: a resize can change the bar's height, and the mark
    // depends on where the line falls.
    addEventListener('resize', () => {
      line = barHeight() + GAP;
      update();
    });
    // Mark on click too, so a jump reads correctly before the scroll settles.
    entries.forEach((entry, i) => entry.link.addEventListener('click', () => mark(i)));
    update();
  }
}
// The page rail scrolls inside its own box, so the current entry can sit well below the
// visible area on a long book. Bring it into view once on load, otherwise the marker that
// says "you are here" is on a part of the rail nobody is looking at.
(() => {
  const active = document.querySelector('.leafsmith-nav-link.leafsmith-active');
  const rail = active && active.closest('.leafsmith-sidebar');
  if (!active || !rail) return;
  // scrollIntoView would move the page as well; only the rail's own scroll position matters.
  //
  // The distance from the rail's top edge is measured from the rendered boxes rather than
  // from offsetTop. A link's offsetTop is relative to its offset parent - the rail, while
  // the rail is positioned - while the rail's own offsetTop is relative to the page, so
  // subtracting the one from the other mixed two coordinate spaces and left the entry short
  // of the middle by the rail's own offset. Reading the boxes measures the one distance
  // that matters and holds whichever way the rail is positioned.
  const railBox = rail.getBoundingClientRect();
  const activeBox = active.getBoundingClientRect();
  const offset = rail.scrollTop + (activeBox.top - railBox.top)
    - (rail.clientHeight - activeBox.height) / 2;
  rail.scrollTop = Math.max(0, offset);
})();
