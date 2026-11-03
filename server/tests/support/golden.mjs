import {utf16Prefix} from './expected.mjs';
// Only strings equal to independent fixture inputs are compacted. A changed/empty value
// remains a different literal and cannot be hidden by the golden normalization.
export function normalizeFixtureResult(value,seeds,embedding){
 const known=new Map();
 const add=(text,label)=>{if(typeof text==='string'&&text.length>80&&!known.has(text))known.set(text,label)};
 add(embedding,{$embedding:true});
 for(const [id,row]of seeds){
  add(row.content,{$fixture:id,field:'content'});
  add(JSON.stringify(row.metadata),{$fixture:id,field:'metadata_json'});
  for(const [key,text]of Object.entries(row.metadata))add(text,{$fixture:id,field:`metadata.${key}`});
  for(const limit of [1200,1201,2000,5000,20000,24000,60000])add(utf16Prefix(row.content,limit),{$fixture:id,field:'content',limit});
 }
 const normalize=(input)=>{
  if(typeof input==='string'){
   if(known.has(input))return known.get(input);
   if(/^[\[{]/u.test(input)){try{const parsed=JSON.parse(input);return {$json:encodePairs(parsed)}}catch{}}
   return input;
  }
  if(Array.isArray(input)){if(JSON.stringify(input)===embedding)return {$embeddingVector:true};return input.map(normalize); }
  if(input&&typeof input==='object')return Object.fromEntries(Object.entries(input).map(([key,item])=>[key,normalize(item)]));
  return input;
 };
 const encodePairs=input=>Array.isArray(input)?input.map(encodePairs):input&&typeof input==='object'?{$pairs:Object.entries(input).map(([key,item])=>[key,encodePairs(item)])}:normalize(input);
 return normalize(value);
}
