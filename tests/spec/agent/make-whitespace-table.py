#!/usr/bin/env python3
"""Freeze Unicode whitespace and Cc/Cf control characters, plus field/position axes."""
import json,pathlib,unicodedata
root=pathlib.Path(__file__).parent
points=[n for n in range(0x110000) if chr(n).isspace() or unicodedata.category(chr(n)) in ('Cc','Cf')]
fields=['cores','coreCount','load','memTotalKb','memTotalB','memAvailKb','pageSize','pagesAvail','diskTotalKb','diskTotalB','diskFreeKb','diskFreeB','cpuLoad','cpuPct','cpu','mount']
table={'schemaVersion':1,'unicodeVersion':unicodedata.unidata_version,'allowlists':{'textBytes':['0x20..0x7e','0x09..0x0d'],'stripAndSplitCodePoints':[9,10,11,12,13,32],'numeric':'-?[0-9]+(\\.[0-9]+)? within the exact integer range'},'codePoints':points,'positions':['start','middle','end'],'platforms':['linux','mac','windows'],'inventoryFields':fields,'inventoryLocations':['key','value'],'signinFields':['settings.homePath','settings.key','settings.driver','credential.expiresAt','credential.refreshTokenExpiresAt','credential.accessToken','credential.refreshToken','credential.key','args.home','args.ccsDir','environment.CLAUDE_CONFIG_DIR','environment.CLAUDE_SECURESTORAGE_CONFIG_DIR'],'policy':'Every case compares the real reference output with the agent: equal or exact fixed fallback; any different ordinary answer fails. All input text is synthetic.'}
(root/'whitespace-controls.json').write_text(json.dumps(table,indent=2)+'\n')
print(len(points),'Unicode whitespace / Cc / Cf points')
