// Following is a reader decision, not a side effect of receiving another token.
export function followViewport(viewport, changed = () => {}) {
  let following = true, unread = 0;
  const nearEnd = () => viewport.scrollHeight - viewport.clientHeight - viewport.scrollTop < 96;
  const scroll = () => { following = nearEnd(); if(following)unread=0;changed(!following&&unread>0); };
  viewport.addEventListener('scroll',scroll,{passive:true});
  return {
    viewport,
    before:()=>following||nearEnd(),
    after(wasFollowing) { if(wasFollowing){viewport.scrollTop=viewport.scrollHeight;following=true;unread=0;}else unread++;changed(!following&&unread>0); },
    latest(){following=true;unread=0;viewport.scrollTop=viewport.scrollHeight;changed(false);},
    dispose(){viewport.removeEventListener('scroll',scroll);},
  };
}
