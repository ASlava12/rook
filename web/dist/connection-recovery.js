// Recovery only probes the daemon and reattaches an observer. It never sends a
// prompt or retries an uncertain write. One timer and one probe belong to a view.
export function connectionRecovery({inspect,reopen,changed,visible=()=>true,schedule=setTimeout,cancel=clearTimeout}) {
  let timer=null,pending=null,epoch=0,attempt=0,disposed=false;
  function clear(){epoch++;if(timer!==null)cancel(timer);timer=null;pending?.abort();pending=null;}
  function plan(){if(disposed)return;const delay=[1000,2000,5000,10000,20000,30000][Math.min(attempt++,5)];attempt=Math.min(attempt,6);timer=schedule(check,delay);}
  async function check(){
    timer=null;if(disposed)return;if(!visible()){plan();return;}
    const token=++epoch,controller=new AbortController();pending=controller;
    const deadline=schedule(()=>controller.abort(),10000);
    let result;
    try{result=await inspect(controller.signal);}catch{result={kind:'offline'};}
    finally{cancel(deadline);if(pending===controller)pending=null;}
    if(disposed||token!==epoch)return;
    if(result.kind==='project'){changed(result);return;}
    if(result.kind==='offline'){changed(result);plan();return;}
    changed({kind:'connecting'});reopen();
  }
  return {
    closed(){clear();if(disposed)return;changed({kind:'offline'});plan();},
    opened(){clear();attempt=0;if(!disposed)changed({kind:'ready'});},
    retry(){clear();attempt=0;if(!disposed){changed({kind:'connecting'});return check();}},
    dispose(){clear();disposed=true;},
  };
}
