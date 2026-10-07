'use strict';
const message = document.getElementById('message');
async function request(url, body, csrf) {
  const response = await fetch(url, {method:body === undefined ? 'GET':'POST', credentials:'same-origin', headers:body === undefined ? {}:{'Content-Type':'application/json', ...(csrf ? {'X-CSRF-Token':csrf}:{})}, ...(body === undefined ? {}:{body:JSON.stringify(body)})});
  if (!response.ok) throw new Error(response.status === 429 ? 'Trop de tentatives. Réessayez dans une minute.' : 'Accès refusé. Vérifiez vos identifiants ou demandez un nouveau lien.');
  return response.json();
}
async function run(button, work) {
  button.disabled=true;message.textContent='';
  try {await work();}
  catch (error) {message.textContent=error.message;}
  finally {button.disabled=false;}
}
const page=document.body.dataset.page;
if (page === 'login') document.getElementById('login').addEventListener('submit', event => {
  event.preventDefault();run(event.target.querySelector('button'), async()=>{
    await request('/auth/password/login',{name:document.getElementById('name').value,password:document.getElementById('password').value});
    document.getElementById('password').value='';location.assign('/');
  });
});
if (page === 'enroll') {
  const token=location.hash.slice(1);history.replaceState(null,'','/enroll');
  const form=document.getElementById('enroll');
  const minimum=Number(document.body.dataset.passwordMinLength);
  const maximum=Number(document.body.dataset.passwordMaxLength);
  if (!token) {message.textContent="Ce lien est incomplet. Demandez un nouveau lien à votre administrateur.";form.hidden=true;}
  form.addEventListener('submit', event=>{
    event.preventDefault();run(form.querySelector('button'),async()=>{
      const password=document.getElementById('password').value;
      if (password !== document.getElementById('confirm').value) throw new Error('Les deux mots de passe doivent être identiques.');
      const length=[...password].length;
      if (length < minimum || length > maximum) throw new Error(`Le mot de passe doit contenir entre ${minimum} et ${maximum} caractères.`);
      await request('/auth/password/enroll',{token,password});
      form.reset();form.hidden=true;message.textContent='Mot de passe enregistré. Vous pouvez vous connecter.';
    });
  });
}
if (page === 'network') request('/api/session').then(context=>{
  document.getElementById('account').textContent=context.name;
  for (const station of context.stations) {
    const card=document.createElement('article');card.className='station';
    const title=document.createElement('h2');title.textContent=station.label;
    const status=document.createElement('p');status.textContent='État indisponible';
    card.append(title,status);document.getElementById('stations').append(card);
  }
  if (!context.stations.length) message.textContent='Aucune station accessible.';
  document.getElementById('logout').addEventListener('click',event=>run(event.target,async()=>{await request('/auth/logout',{},context.csrf_token);location.assign('/login');}));
}).catch(()=>location.assign('/login'));
