// Public DTO expectations are built from fixture inputs, never the production display helpers.
export function utf16Prefix(text,limit){let result='';for(const scalar of text){if(result.length+scalar.length>limit)break;result+=scalar}return result}
export function shortSummary(text,limit=300){return text.length<=limit?text:utf16Prefix(text,limit)+'…'}
export function provenanceOf(record){return {containerTag:record.containerTag,...(typeof record.metadata.sm_project_id==='string'?{projectId:record.metadata.sm_project_id}:{}),...(typeof record.metadata.filepath==='string'?{filepath:record.metadata.filepath}:{})}}
export function documentExpected(record,projection='full',get=false){
 const {id,metadata,content,topics=[],createdAt,updatedAt=createdAt,status='done'}=record;
 if(projection==='ids')return {id};if(projection==='capture')return {id,metadata};
 const value={id,title:metadata.title??`Memory ${id.slice(0,8)}`,type:'text',status,createdAt,updatedAt,
  summary:projection==='index'?[...content.split('\0')[0]].slice(0,300).join(''):shortSummary(content),
  ...(projection==='full'?{content}:{}),containerTags:[record.containerTag],metadata,topics,provenance:provenanceOf(record)};
 if(projection==='full')value.enrichment={embeddingStatus:'done',factStatus:'done',vectorStatus:'pending',embeddingModel:null,factModel:null,topicStatus:'done',topicModel:null,...(get?{embeddedAt:null,factsExtractedAt:null}:{}),topicsExtractedAt:null,...(get?{vectorAttemptedAt:null}:{}),error:null,...record.enrichment};
 if(!get){value.isForgotten=false;value.isLatest=true;}return value;
}
export function listExpected(records,{projection='full',page=1,limit=10,total=records.length}={}){
 const documents=records.map(record=>documentExpected(record,projection));
 return {documents,...(projection==='full'?{memoryEntries:records.map(record=>({id:record.id,memory:record.content,content:record.content,isForgotten:false,isLatest:true,createdAt:record.createdAt,updatedAt:record.updatedAt??record.createdAt,topics:record.topics??[],provenance:provenanceOf(record)}))}:{}),pagination:{currentPage:page,limit,totalItems:total,totalPages:Math.ceil(total/limit)}};
}
