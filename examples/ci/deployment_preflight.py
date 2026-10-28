#!/usr/bin/env python3
"""Fail-closed deployment proposal checks and local verified export.

The expected configuration fingerprint and approval record must come from a
trusted operator channel. Self-reported booleans/hashes are not authentication.
This program never downloads binaries, runs actions or uploads artifacts.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import sys
from urllib.parse import urlsplit

PINS={'actions/checkout':'3d3c42e5aac5ba805825da76410c181273ba90b1',
      'actions/upload-artifact':'cf430e030ddbb5b0abf93d22962f4752f3646cd9'}


def fingerprint(config):
    return hashlib.sha256(json.dumps(config,sort_keys=True,separators=(',', ':'),ensure_ascii=False,
                                     allow_nan=False).encode()).hexdigest()


def hex_value(value, lengths):
    return type(value) is str and len(value) in lengths and bool(re.fullmatch('[0-9a-f]+',value))


def https_url(value):
    if type(value) is not str:return False
    parsed=urlsplit(value)
    return parsed.scheme=='https' and bool(parsed.hostname) and parsed.username is None and parsed.password is None and not parsed.query and not parsed.fragment


def blockers(plan, expected=None):
    problems=[]
    def need(ok, message):
        if not ok:problems.append(message)
    config=plan.get('configuration',{});auth=plan.get('authorization',{})
    actual=fingerprint(config)
    need(plan.get('configuration_sha256')==actual,'configuration fingerprint mismatch')
    need(expected==actual,'independently trusted expected configuration fingerprint missing or changed')
    need(auth.get('hosted_execution_approved') is True,'hosted execution is not authorized')
    need(auth.get('approved_configuration_sha256')==actual,'approval does not bind this configuration')
    need(bool(auth.get('approval_record')),'separate approval record missing')
    actions=config.get('actions',{})
    need(set(actions)==set(PINS),'unexpected or missing external action')
    for repo,sha in PINS.items():
        row=actions.get(repo,{})
        need(row.get('commit')==sha and hex_value(row.get('commit'),(40,)) and row.get('reviewed') is True,
             'unreviewed/invalid action revision: '+repo)
    workflow=config.get('workflow',{})
    need(workflow.get('event')=='workflow_dispatch','initial pilot must be manual workflow_dispatch')
    need(workflow.get('permissions')=={'contents':'read'},'permissions must be contents:read only')
    need(workflow.get('persist_credentials') is False and workflow.get('detached_checkout') is True,'unsafe checkout settings')
    need(workflow.get('project_scripts') is False and workflow.get('fork_policy')=='deny','project scripts and fork publication must be denied')
    need(workflow.get('runner')=='ubuntu-24.04','runner proposal changed')
    need(hex_value(workflow.get('source_revision'),(40,)) and workflow.get('source_reviewed') is True,'reviewed workflow source revision unresolved')
    target=config.get('target',{})
    repository=target.get('repository')
    need(type(repository) is str and bool(re.fullmatch(r'[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+',repository)),'hosted repository identity unresolved')
    need(target.get('private') is True and target.get('is_fork') is False,'private same-repository target required; fork/unknown metadata refused')
    for side in ('base','head'):
        need(hex_value(target.get(side),(40,64)),'explicit full '+side+' commit required')
    exe=config.get('executable',{})
    need(exe.get('strategy')=='trusted-release','select the proposed trusted-release strategy')
    need(hex_value(exe.get('source_revision'),(40,)) and exe.get('source_reviewed') is True,'reviewed Atlas source revision unresolved')
    need(https_url(exe.get('url')),'trusted credential-free HTTPS executable distribution unresolved')
    need(hex_value(exe.get('sha256'),(64,)),'hosted executable checksum unresolved')
    need(exe.get('provenance_approved') is True and bool(exe.get('independent_trust_anchor')),'independent executable provenance not approved')
    need(bool(exe.get('build_environment')) and bool(exe.get('dependency_lock_sha256')),'build/dependency provenance unresolved')
    verifier=config.get('verifier',{})
    need(hex_value(verifier.get('source_revision'),(40,)) and verifier.get('source_reviewed') is True,'trusted verifier source revision unresolved')
    need(hex_value(verifier.get('consumer_sha256'),(64,)) and hex_value(verifier.get('ci_helper_sha256'),(64,)),
         'trusted independent consumer/helper checksums unresolved')
    need(hex_value(verifier.get('hash_provider_sha256'),(64,)) and verifier.get('hash_provider_approved') is True,'independent BLAKE3 provider not approved')
    need(https_url(verifier.get('hash_provider_url')),'independent hash provider HTTPS distribution unresolved')
    publication=config.get('publication',{})
    need(publication.get('authorized') is True,'artifact publication is not authorized')
    need(repository in publication.get('allowed_repositories',[]) if repository else False,'target repository is not approved for publication')
    need(publication.get('visibility')=='repository-readers' and publication.get('visibility_approved') is True,
         'artifact visibility must be explicitly approved')
    need(publication.get('retention_days')==7 and publication.get('retention_approved') is True,'seven-day retention not approved')
    need(publication.get('fork_artifacts') is False and publication.get('privacy_reviewed') is True,'fork/privacy policy unresolved')
    need(publication.get('destination')=='current-private-repository-run','artifact destination must stay within the approved private run')
    need(publication.get('retain_identities_via')=='separate-protected-approval-record','trusted identity retention channel unresolved')
    limits=config.get('limits',{})
    need(type(limits.get('hosted_executions')) is int and limits['hosted_executions']==1,'exactly one hosted execution proposed')
    need(limits.get('charges_acknowledged') is True,'possible hosting charges not acknowledged')
    need(limits.get('timeout_minutes')==15,'15-minute runner timeout required')
    return problems


def generate(plan, expected, binary, root, output, hash_provider):
    problems=blockers(plan,expected)
    if problems:raise ValueError('; '.join(problems))
    config=plan['configuration'];binary=Path(binary).resolve();root=Path(root).resolve();output=Path(output)
    protected=Path(__file__).resolve().parents[2]/'benchmarks/results'
    if output.resolve().is_relative_to(protected.resolve()):raise ValueError('preserve historical benchmark outputs')
    if binary.is_relative_to(root) or output.resolve().is_relative_to(root):raise ValueError('trusted executable/output must be outside analysed root')
    if hashlib.sha256(binary.read_bytes()).hexdigest()!=config['executable']['sha256']:raise ValueError('executable checksum mismatch')
    hash_provider=Path(hash_provider).resolve()
    if hash_provider.is_relative_to(root) or hashlib.sha256(hash_provider.read_bytes()).hexdigest()!=config['verifier']['hash_provider_sha256']:
        raise ValueError('independent hash provider checksum/boundary mismatch')
    trusted=Path(__file__).resolve().parents[1]/'manifest_consumer'
    for name,key in [('consumer.py','consumer_sha256'),('ci_artifact.py','ci_helper_sha256')]:
        if hashlib.sha256((trusted/name).read_bytes()).hexdigest()!=config['verifier'][key]:raise ValueError('trusted verifier checksum mismatch')
    git=['git','--no-optional-locks','-c','core.fsmonitor=false','-c','core.hooksPath=/dev/null','-C',str(root)]
    actual=subprocess.check_output([*git,'rev-parse','HEAD'],text=True).strip()
    if actual!=config['target']['head']:raise ValueError('checkout does not match approved head')
    if subprocess.run([*git,'symbolic-ref','-q','HEAD'],capture_output=True).returncode!=1:raise ValueError('detached checkout required')
    sys.path.insert(0,str(trusted))
    from ci_artifact import generate as export
    old=os.environ.get('ATLAS_BLAKE3_COMMAND');os.environ['ATLAS_BLAKE3_COMMAND']=str(hash_provider)
    try:
        return export(binary,config['executable']['sha256'],root,config['target']['base'],config['target']['head'],output)
    finally:
        if old is None:os.environ.pop('ATLAS_BLAKE3_COMMAND',None)
        else:os.environ['ATLAS_BLAKE3_COMMAND']=old


def main():
    p=argparse.ArgumentParser(description=__doc__)
    p.add_argument('--plan',type=Path,required=True);p.add_argument('--expected-config-sha256')
    p.add_argument('--binary',type=Path);p.add_argument('--root',type=Path);p.add_argument('--output',type=Path)
    p.add_argument('--hash-provider',type=Path)
    a=p.parse_args();plan=json.loads(a.plan.read_text());errors=blockers(plan,a.expected_config_sha256)
    if errors:
        print(json.dumps(dict(ready=False,blockers=errors,configuration_sha256=fingerprint(plan.get('configuration',{}))),sort_keys=True))
        return 2
    if a.binary is not None:
        if None in (a.root,a.output,a.hash_provider):p.error('export requires --root, --output and --hash-provider')
        try:print(json.dumps(generate(plan,a.expected_config_sha256,a.binary,a.root,a.output,a.hash_provider),sort_keys=True))
        except Exception as error:
            print(str(error),file=sys.stderr);return 1
    else:print(json.dumps(dict(ready=True,configuration_sha256=fingerprint(plan['configuration'])),sort_keys=True))
    return 0


if __name__=='__main__':sys.exit(main())
