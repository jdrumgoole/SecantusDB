# reference-version: 15
# The multi-byte encodings, as PostgreSQL 15.19 verifies and converts them: convert_from / convert_to / convert / length(bytea, name), and client_encoding. The characters are samples of what tools/pg_mbcs_tables.py measures whole.
# EUC_JP: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61cdfba3e88fbdbe8fe2e5adeec5a8d3dda4e4b3ac8ff4f58fd9b4c3b47a'::bytea, 'EUC_JP'), length('\x61cdfba3e88fbdbe8fe2e5adeec5a8d3dda4e4b3ac8ff4f58fd9b4c3b47a'::bytea, 'EUC_JP')
SELECT convert_from('\x618fdbd4eaa2d3eabdfbe4f88fb3e58fddf4e8a1daf6eec4d7f5e6a27a'::bytea, 'EUC_JP'), length('\x618fdbd4eaa2d3eabdfbe4f88fb3e58fddf4e8a1daf6eec4d7f5e6a27a'::bytea, 'EUC_JP')
SELECT convert_from('\x61d1f0cdd1e7f68fedcde0dbd0f9dfba8fe1a98fbbd7f3bbd2c08fc7a57a'::bytea, 'EUC_JP'), length('\x61d1f0cdd1e7f68fedcde0dbd0f9dfba8fe1a98fbbd7f3bbd2c08fc7a57a'::bytea, 'EUC_JP')
SELECT convert_to('示畝粮億秈蓷ｐ渶眼蕻楅錚', 'EUC_JP'), convert_from(convert_to('示畝粮億秈蓷ｐ渶眼蕻楅錚', 'EUC_JP'), 'EUC_JP') = '示畝粮億秈蓷ｐ渶眼蕻楅錚'
SELECT convert_to('踹碩珒鶵俉粽А嵡土膺媼詁', 'EUC_JP'), convert_from(convert_to('踹碩珒鶵俉粽А嵡土膺媼詁', 'EUC_JP'), 'EUC_JP') = '踹碩珒鶵俉粽А嵡土膺媼詁'
SELECT convert_to('黧銽雉φ晳腋紬塢勨呰犹丂', 'EUC_JP'), convert_from(convert_to('黧銽雉φ晳腋紬塢勨呰犹丂', 'EUC_JP'), 'EUC_JP') = '黧銽雉φ晳腋紬塢勨呰犹丂'
SELECT convert_from('\x78acfd'::bytea, 'EUC_JP')
SELECT convert_from('\x788ff7d6'::bytea, 'EUC_JP')
SELECT convert_from('\x8ef7a1a1'::bytea, 'EUC_JP')
SELECT convert_from('\x8efaa1a1'::bytea, 'EUC_JP')
SELECT convert_from('\xa1a2a2af5c783830'::bytea, 'EUC_JP')
SELECT length('\xa2af'::bytea, 'EUC_JP')
SELECT convert_from('\xa1'::bytea, 'EUC_JP')
SELECT convert_from('\x6162a1a400'::bytea, 'EUC_JP')
SELECT convert_from('\x5c7838645c783230'::bytea, 'EUC_JP')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'EUC_JP')
SELECT length('\xa171'::bytea, 'EUC_JP')
SELECT convert_to('aéb', 'EUC_JP')
SELECT convert_to('😀', 'EUC_JP')
SELECT convert_to('あ́', 'EUC_JP')
# EUC_CN: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61d9b0c2d2cbe5d4cfbcfeb8c3d8b1d4bfc7d7a9c7bdb8c3e97a'::bytea, 'EUC_CN'), length('\x61d9b0c2d2cbe5d4cfbcfeb8c3d8b1d4bfc7d7a9c7bdb8c3e97a'::bytea, 'EUC_CN')
SELECT convert_from('\x61c3f2b7a8f0e1dfc3c4cecaccf7ded2bac6a4f4dccfb6ecf87a'::bytea, 'EUC_CN'), length('\x61c3f2b7a8f0e1dfc3c4cecaccf7ded2bac6a4f4dccfb6ecf87a'::bytea, 'EUC_CN')
SELECT convert_from('\x61a9a5eaebf7f4f1eceeb2c0ced2a9b5adb2ece3e6d3dfcae47a'::bytea, 'EUC_CN'), length('\x61a9a5eaebf7f4f1eceeb2c0ced2a9b5adb2ece3e6d3dfcae47a'::bytea, 'EUC_CN')
SELECT convert_to('枸к霉巯贿伫怎ロ◆滤鼹ヅ', 'EUC_CN'), convert_from(convert_to('枸к霉巯贿伫怎ロ◆滤鼹ヅ', 'EUC_CN'), 'EUC_CN') = '枸к霉巯贿伫怎ロ◆滤鼹ヅ'
SELECT convert_to('崛员门甄笫狞鲈瓞〈甑ヘ爽', 'EUC_CN'), convert_from(convert_to('崛员门甄笫狞鲈瓞〈甑ヘ爽', 'EUC_CN'), 'EUC_CN') = '崛员门甄笫狞鲈瓞〈甑ヘ爽'
SELECT convert_to('鞅井箔炬孕遨蹁绻滏蜩评崩', 'EUC_CN'), convert_from(convert_to('鞅井箔炬孕遨蹁绻滏蜩评崩', 'EUC_CN'), 'EUC_CN') = '鞅井箔炬孕遨蹁绻滏蜩评崩'
SELECT convert_from('\x78fefd'::bytea, 'EUC_CN')
SELECT convert_from('\x78aaaa'::bytea, 'EUC_CN')
SELECT convert_from('\xa1a2a2a15c783830'::bytea, 'EUC_CN')
SELECT length('\xa2a1'::bytea, 'EUC_CN')
SELECT convert_from('\xa1'::bytea, 'EUC_CN')
SELECT convert_from('\x6162a1a400'::bytea, 'EUC_CN')
SELECT convert_from('\x5c7838645c783230'::bytea, 'EUC_CN')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'EUC_CN')
SELECT length('\xa171'::bytea, 'EUC_CN')
SELECT convert_to('aéb', 'EUC_CN')
SELECT convert_to('😀', 'EUC_CN')
SELECT convert_to('あ́', 'EUC_CN')
# EUC_KR: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61a2d5aca2e0d3e8e0f2a6e3a2abf0dbb1f4afdceda7b1f1d97a'::bytea, 'EUC_KR'), length('\x61a2d5aca2e0d3e8e0f2a6e3a2abf0dbb1f4afdceda7b1f1d97a'::bytea, 'EUC_KR')
SELECT convert_from('\x61e9d9a3f1e3afeab2a1cfbed5d2b6d1fafdcbe0b1dfaad0fc7a'::bytea, 'EUC_KR'), length('\x61e9d9a3f1e3afeab2a1cfbed5d2b6d1fafdcbe0b1dfaad0fc7a'::bytea, 'EUC_KR')
SELECT convert_from('\x61ddc1fcb5dea3a1cae4c8cde4e1e7acdcc3cfb8c1eca5e0ef7a'::bytea, 'EUC_KR'), length('\x61ddc1fcb5dea3a1cae4c8cde4e1e7acdcc3cfb8c1eca5e0ef7a'::bytea, 'EUC_KR')
SELECT convert_to('悧齟夏阜굳탉壘式벧Τ奬暴', 'EUC_KR'), convert_from(convert_to('悧齟夏阜굳탉壘式벧Τ奬暴', 'EUC_KR'), 'EUC_KR') = '悧齟夏阜굳탉壘式벧Τ奬暴'
SELECT convert_to('툭板烘韜듀섐笠巫펩曳걺턴', 'EUC_KR'), convert_from(convert_to('툭板烘韜듀섐笠巫펩曳걺턴', 'EUC_KR'), 'EUC_KR') = '툭板烘韜듀섐笠巫펩曳걺턴'
SELECT convert_to('셩♬蛟강想蜚넷湲伶퍅甚奇', 'EUC_KR'), convert_from(convert_to('셩♬蛟강想蜚넷湲伶퍅甚奇', 'EUC_KR'), 'EUC_KR') = '셩♬蛟강想蜚넷湲伶퍅甚奇'
SELECT convert_from('\x78a6f9'::bytea, 'EUC_KR')
SELECT convert_from('\x78aeb9'::bytea, 'EUC_KR')
SELECT convert_from('\xa1a2a2e95c783830'::bytea, 'EUC_KR')
SELECT length('\xa2e9'::bytea, 'EUC_KR')
SELECT convert_from('\xa1'::bytea, 'EUC_KR')
SELECT convert_from('\x6162a1a400'::bytea, 'EUC_KR')
SELECT convert_from('\x5c7838645c783230'::bytea, 'EUC_KR')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'EUC_KR')
SELECT length('\xa171'::bytea, 'EUC_KR')
SELECT convert_to('aéb', 'EUC_KR')
SELECT convert_to('😀', 'EUC_KR')
SELECT convert_to('あ́', 'EUC_KR')
# EUC_TW: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x618ea1c8b4d7e68ea2bfa48ea2def0f7bd8ea1d0dc8ea1f0fdf5eed2cf8ea2a4db8ea2b5f98ea1cfcf7a'::bytea, 'EUC_TW'), length('\x618ea1c8b4d7e68ea2bfa48ea2def0f7bd8ea1d0dc8ea1f0fdf5eed2cf8ea2a4db8ea2b5f98ea1cfcf7a'::bytea, 'EUC_TW')
SELECT convert_from('\x618ea2b6dafac58ea2a6a28ea2eae3f2b38ea2d2fdfbcf8ea2cbae8ea2c9b68ea2c7cc8ea2c7bbfae37a'::bytea, 'EUC_TW'), length('\x618ea2b6dafac58ea2a6a28ea2eae3f2b38ea2d2fdfbcf8ea2cbae8ea2c9b68ea2c7cc8ea2c7bbfae37a'::bytea, 'EUC_TW')
SELECT convert_from('\x618ea2bafdd6f78ea2b7e48ea1e0b58ea2beb88ea1d6eec7c48ea2c1e98ea1f4d98ea2b1befaa2cafa7a'::bytea, 'EUC_TW'), length('\x618ea2bafdd6f78ea2b7e48ea1e0b58ea2beb88ea1d6eec7c48ea2c1e98ea1f4d98ea2b1befaa2cafa7a'::bytea, 'EUC_TW')
SELECT convert_to('␁釗辵儾縩邟鉻猱蛗鄛輙裾', 'EUC_TW'), convert_from(convert_to('␁釗辵儾縩邟鉻猱蛗鄛輙裾', 'EUC_TW'), 'EUC_TW') = '␁釗辵儾縩邟鉻猱蛗鄛輙裾'
SELECT convert_to('饔彝敓立遾閜給錙鱳妎汊洯', 'EUC_TW'), convert_from(convert_to('饔彝敓立遾閜給錙鱳妎汊洯', 'EUC_TW'), 'EUC_TW') = '饔彝敓立遾閜給錙鱳妎汊洯'
SELECT convert_to('日騎蠡崌蠕綋籦遳爆暾瑺詛', 'EUC_TW'), convert_from(convert_to('日騎蠡崌蠕綋籦遳爆暾瑺詛', 'EUC_TW'), 'EUC_TW') = '日騎蠡崌蠕綋籦遳爆暾瑺詛'
SELECT convert_from('\x788ea1b9fc'::bytea, 'EUC_TW')
SELECT convert_from('\x788ea4aba8'::bytea, 'EUC_TW')
SELECT convert_from('\x8eabb9a4a1a1'::bytea, 'EUC_TW')
SELECT convert_from('\x8eaca1c5a1a1'::bytea, 'EUC_TW')
SELECT convert_from('\xa1a280a15c783830'::bytea, 'EUC_TW')
SELECT length('\x80a1'::bytea, 'EUC_TW')
SELECT convert_from('\xa1'::bytea, 'EUC_TW')
SELECT convert_from('\x6162a1a400'::bytea, 'EUC_TW')
SELECT convert_from('\x5c7838645c783230'::bytea, 'EUC_TW')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'EUC_TW')
SELECT length('\xa171'::bytea, 'EUC_TW')
SELECT convert_to('aéb', 'EUC_TW')
SELECT convert_to('😀', 'EUC_TW')
SELECT convert_to('あ́', 'EUC_TW')
# SJIS: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x618c4b9e6a9072fada91af8ba998cbe1ef9b8f8f8e8b949a5b7a'::bytea, 'SJIS'), length('\x618c4b9e6a9072fada91af8ba998cbe1ef9b8f8f8e8b949a5b7a'::bytea, 'SJIS')
SELECT convert_from('\x61fc4a9bca8a62e1c68f9de7c28e87965c97b28eaa9ff691927a'::bytea, 'SJIS'), length('\x61fc4a9bca8a62e1c68f9de7c28e87965c97b28eaa9ff691927a'::bytea, 'SJIS')
SELECT convert_from('\x61e8b796c3e3c5e17599b8e196e0a1e2b8969be5a4e898ed9e7a'::bytea, 'SJIS'), length('\x61e8b796c3e3c5e17599b8e196e0a1e2b8969be5a4e898ed9e7a'::bytea, 'SJIS')
SELECT convert_to('狐鈔展擂陰極朸晰椹散蚩旛', 'SJIS'), convert_from(convert_to('狐鈔展擂陰極朸晰椹散蚩旛', 'SJIS'), 'SJIS') = '狐鈔展擂陰極朸晰椹散蚩旛'
SELECT convert_to('傍鈩匳疋屓融蹤岦紊薤顧享', 'SJIS'), convert_from(convert_to('傍鈩匳疋屓融蹤岦紊薤顧享', 'SJIS'), 'SJIS') = '傍鈩匳疋屓融蹤岦紊薤顧享'
SELECT convert_to('埴規祀認挂弼家巧疲廣略鯛', 'SJIS'), convert_from(convert_to('埴規祀認挂弼家巧疲廣略鯛', 'SJIS'), 'SJIS') = '埴規祀認挂弼家巧疲廣略鯛'
SELECT convert_from('\x788870'::bytea, 'SJIS')
SELECT convert_from('\x78fc82'::bytea, 'SJIS')
SELECT convert_from('\xe67fa1'::bytea, 'SJIS')
SELECT convert_from('\xffb1a1'::bytea, 'SJIS')
SELECT convert_from('\xa281ad5c783830'::bytea, 'SJIS')
SELECT length('\x81ad'::bytea, 'SJIS')
SELECT convert_from('\xa3'::bytea, 'SJIS')
SELECT convert_from('\x6162a400'::bytea, 'SJIS')
SELECT convert_from('\x5c7838645c783230'::bytea, 'SJIS')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'SJIS')
SELECT length('\xa571'::bytea, 'SJIS')
SELECT convert_to('aéb', 'SJIS')
SELECT convert_to('😀', 'SJIS')
SELECT convert_to('あ́', 'SJIS')
# BIG5: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61c955b25dd8c4e646e75ab7e1dce1d9e8e1b0f2f5e4c7f4d27a'::bytea, 'BIG5'), length('\x61c955b25dd8c4e646e75ab7e1dce1d9e8e1b0f2f5e4c7f4d27a'::bytea, 'BIG5')
SELECT convert_from('\x61ecd5c3d1e37bad5af74ce0e0a5debcccf2b0a7ccb8c0a5ab7a'::bytea, 'BIG5'), length('\x61ecd5c3d1e37bad5af74ce0e0a5debcccf2b0a7ccb8c0a5ab7a'::bytea, 'BIG5')
SELECT convert_from('\x61c66dcbb0c3e1d8cbc071dffde4fadfc7c2a7c346c74ccea57a'::bytea, 'BIG5'), length('\x61c66dcbb0c3e1d8cbc071dffde4fadfc7c2a7c346c74ccea57a'::bytea, 'BIG5')
SELECT convert_to('豤芐褽烼隬℃穔闍萉訄扡璆', 'BIG5'), convert_from(convert_to('豤芐褽烼隬℃穔闍萉訄扡璆', 'BIG5'), 'BIG5') = '豤芐褽烼隬℃穔闍萉訄扡璆'
SELECT convert_to('邏冗踇屯癰奼埳儿裱呯侉襼', 'BIG5'), convert_from(convert_to('邏冗踇屯癰奼埳儿裱呯侉襼', 'BIG5'), 'BIG5') = '邏冗踇屯癰奼埳儿裱呯侉襼'
SELECT convert_to('燇阯蜈背鳲瘵滲嫿彙胑涃黟', 'BIG5'), convert_from(convert_to('燇阯蜈背鳲瘵滲嫿彙胑涃黟', 'BIG5'), 'BIG5') = '燇阯蜈背鳲瘵滲嫿彙胑涃黟'
SELECT convert_from('\x788bad'::bytea, 'BIG5')
SELECT convert_from('\x7891de'::bytea, 'BIG5')
SELECT convert_from('\x8d20a140'::bytea, 'BIG5')
SELECT convert_from('\xa14180015c783830'::bytea, 'BIG5')
SELECT length('\x8001'::bytea, 'BIG5')
SELECT convert_from('\xa1'::bytea, 'BIG5')
SELECT convert_from('\x6162a14300'::bytea, 'BIG5')
SELECT convert_from('\x5c7838645c783230'::bytea, 'BIG5')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'BIG5')
SELECT length('\xa171'::bytea, 'BIG5')
SELECT convert_to('aéb', 'BIG5')
SELECT convert_to('😀', 'BIG5')
SELECT convert_to('あ́', 'BIG5')
# GBK: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61c487e2b1b58ee18de045897bd46adee486459a488276ed8b7a'::bytea, 'GBK'), length('\x61c487e2b1b58ee18de045897bd46adee486459a488276ed8b7a'::bytea, 'GBK')
SELECT convert_from('\x61d48fdba9ad5499ec9e73dfe0f394cbb9b1ccac65d1f3c1be7a'::bytea, 'GBK'), length('\x61d48fdba9ad5499ec9e73dfe0f394cbb9b1ccac65d1f3c1be7a'::bytea, 'GBK')
SELECT convert_from('\x61fb48dde9896e95dbe6b8a0bbccf8eb8ab970bee4f4d093c77a'::bytea, 'GBK'), length('\x61fb48dde9896e95dbe6b8a0bbccf8eb8ab970bee4f4d093c77a'::bytea, 'GBK')
SELECT convert_to('淼弌赲╣橫氀萆嵾縇漸鰔呻', 'GBK'), convert_from(convert_to('淼弌赲╣橫氀萆嵾縇漸鰔呻', 'GBK'), 'GBK') = '淼弌赲╣橫氀萆嵾縇漸鰔呻'
SELECT convert_to('鬨蹕篇枙出誓骈矒惢戎摙妦', 'GBK'), convert_from(convert_to('鬨蹕篇枙出誓骈矒惢戎摙妦', 'GBK'), 'GBK') = '鬨蹕篇枙出誓骈矒惢戎摙妦'
SELECT convert_to('塊焰穉挪槗尙镔烸芪屃鎩裠', 'GBK'), convert_from(convert_to('塊焰穉挪槗尙镔烸芪屃鎩裠', 'GBK'), 'GBK') = '塊焰穉挪槗尙镔烸芪屃鎩裠'
SELECT convert_from('\x78ac07'::bytea, 'GBK')
SELECT convert_from('\x78822b'::bytea, 'GBK')
SELECT convert_from('\x8d208140'::bytea, 'GBK')
SELECT convert_from('\x814180015c783830'::bytea, 'GBK')
SELECT length('\x8001'::bytea, 'GBK')
SELECT convert_from('\x81'::bytea, 'GBK')
SELECT convert_from('\x6162814300'::bytea, 'GBK')
SELECT convert_from('\x5c7838645c783230'::bytea, 'GBK')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'GBK')
SELECT length('\x8171'::bytea, 'GBK')
SELECT convert_to('aéb', 'GBK')
SELECT convert_to('😀', 'GBK')
SELECT convert_to('あ́', 'GBK')
# UHC: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61fbd68841c9fddcb49081efc5c0d2a059c7b68b56e8b38b797a'::bytea, 'UHC'), length('\x61fbd68841c9fddcb49081efc5c0d2a059c7b68b56e8b38b797a'::bytea, 'UHC')
SELECT convert_from('\x61f0e8c0dc9fb1af4dd5aec7b2e0f1d2b1c7d0cbd48da19bad7a'::bytea, 'UHC'), length('\x61f0e8c0dc9fb1af4dd5aec7b2e0f1d2b1c7d0cbd48da19bad7a'::bytea, 'UHC')
SELECT convert_from('\x61b692af4697b592ac8dc0a9acd4dda845bd838f4cdecbc58e7a'::bytea, 'UHC'), length('\x61b692af4697b592ac8dc0a9acd4dda845bd838f4cdecbc58e7a'::bytea, 'UHC')
SELECT convert_to('뉮唇윯뀔첖쮻コてⅲ諱櫂滉', 'UHC'), convert_from(convert_to('뉮唇윯뀔첖쮻コてⅲ諱櫂滉', 'UHC'), 'UHC') = '뉮唇윯뀔첖쮻コてⅲ諱櫂滉'
SELECT convert_to('폩뽪궸壟쬷톍址笙爻제酷ㅑ', 'UHC'), convert_from(convert_to('폩뽪궸壟쬷톍址笙爻제酷ㅑ', 'UHC'), 'UHC') = '폩뽪궸壟쬷톍址笙爻제酷ㅑ'
SELECT convert_to('臥햢뫝봍붇뾁엒턋흒뗼懲逝', 'UHC'), convert_from(convert_to('臥햢뫝봍붇뾁엒턋흒뗼懲逝', 'UHC'), 'UHC') = '臥햢뫝봍붇뾁엒턋흒뗼懲逝'
SELECT convert_from('\x78ba18'::bytea, 'UHC')
SELECT convert_from('\x78c710'::bytea, 'UHC')
SELECT convert_from('\x8d208141'::bytea, 'UHC')
SELECT convert_from('\x814280015c783830'::bytea, 'UHC')
SELECT length('\x8001'::bytea, 'UHC')
SELECT convert_from('\x81'::bytea, 'UHC')
SELECT convert_from('\x6162814400'::bytea, 'UHC')
SELECT convert_from('\x5c7838645c783230'::bytea, 'UHC')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'UHC')
SELECT length('\x8171'::bytea, 'UHC')
SELECT convert_to('aéb', 'UHC')
SELECT convert_to('😀', 'UHC')
SELECT convert_to('あ́', 'UHC')
# GB18030: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61dd38c935d139ac39b834b935e238df37a736d331be358831d437f636bc31c838af35ae32ba6fc833ec38ca34a5317a'::bytea, 'GB18030'), length('\x61dd38c935d139ac39b834b935e238df37a736d331be358831d437f636bc31c838af35ae32ba6fc833ec38ca34a5317a'::bytea, 'GB18030')
SELECT convert_from('\x61b131d030ce31f033c933ea34b532ad3697388234faedac31df3296329630c7339d309032b435bc37c433e037b1337a'::bytea, 'GB18030'), length('\x61b131d030ce31f033c933ea34b532ad3697388234faedac31df3296329630c7339d309032b435bc37c433e037b1337a'::bytea, 'GB18030')
SELECT convert_from('\x61b137f739bc37e3338232ee32d96ea936d230a335a530a131b3359530bf31bf399234ac33a736bf37ee399835cf357a'::bytea, 'GB18030'), length('\x61b137f739bc37e3338232ee32d96ea936d230a335a530a131b3359530bf31bf399234ac33a736bf37ee399835cf357a'::bytea, 'GB18030')
SELECT convert_to('򉡬񍦼􈻏𙘉񴪄􋌏뺅񚋣𶁟򃒠񭀡񢱹', 'GB18030'), convert_from(convert_to('򉡬񍦼􈻏𙘉񴪄􋌏뺅񚋣𶁟򃒠񭀡񢱹', 'GB18030'), 'GB18030') = '򉡬񍦼􈻏𙘉񴪄􋌏뺅񚋣𶁟򃒠񭀡񢱹'
SELECT convert_to('󚰓򔱚񁤴􈖎񺖊񊽒㍴򤊆􂫀𛟡񄕼㻥', 'GB18030'), convert_from(convert_to('󚰓򔱚񁤴􈖎񺖊񊽒㍴򤊆􂫀𛟡񄕼㻥', 'GB18030'), 'GB18030') = '󚰓򔱚񁤴􈖎񺖊񊽒㍴򤊆􂫀𛟡񄕼㻥'
SELECT convert_to('򇪬񊮶𭧲򧧦񄂳񫖌򿕼𵬕󇤸񑏵𣄥𚢈', 'GB18030'), convert_from(convert_to('򇪬񊮶𭧲򧧦񄂳񫖌򿕼𵬕󇤸񑏵𣄥𚢈', 'GB18030'), 'GB18030') = '򇪬񊮶𭧲򧧦񄂳񫖌򿕼𵬕󇤸񑏵𣄥𚢈'
SELECT convert_from('\x78ed328f38'::bytea, 'GB18030')
SELECT convert_from('\x78ef32e536'::bytea, 'GB18030')
SELECT convert_from('\x81418431a5305c783830'::bytea, 'GB18030')
SELECT length('\x8431a530'::bytea, 'GB18030')
SELECT convert_from('\x81'::bytea, 'GB18030')
SELECT convert_from('\x6162814300'::bytea, 'GB18030')
SELECT convert_from('\x5c7838645c783230'::bytea, 'GB18030')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'GB18030')
SELECT length('\x8171'::bytea, 'GB18030')
SELECT convert_to('aéb', 'GB18030')
SELECT convert_to('😀', 'GB18030')
SELECT convert_to('あ́', 'GB18030')
# JOHAB: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x619afaedbdf3b8a4abf0bca8d7e5ebcca4c4dc9ccecaadd3bc7a'::bytea, 'JOHAB'), length('\x619afaedbdf3b8a4abf0bca8d7e5ebcca4c4dc9ccecaadd3bc7a'::bytea, 'JOHAB')
SELECT convert_from('\x61e4f0f3cf9defadc4eef59cacc2f8efe3e4e18dead9dcedc17a'::bytea, 'JOHAB'), length('\x61e4f0f3cf9defadc4eef59cacc2f8efe3e4e18dead9dcedc17a'::bytea, 'JOHAB')
SELECT convert_from('\x618ed7b9b595a4e8e49cd6b4b1c8d0f6dfccc1dad5cde4c2a47a'::bytea, 'JOHAB'), length('\x618ed7b9b595a4e8e49cd6b4b1c8d0f6dfccc1dad5cde4c2a47a'::bytea, 'JOHAB')
SELECT convert_to('支텤뒿퍴뼻돤돋웜設봉뜏耽', 'JOHAB'), convert_from(convert_to('支텤뒿퍴뼻돤돋웜設봉뜏耽', 'JOHAB'), 'JOHAB') = '支텤뒿퍴뼻돤돋웜設봉뜏耽'
SELECT convert_to('벁먽듎핋쓑햡썩촜펁턏했쿨', 'JOHAB'), convert_from(convert_to('벁먽듎핋쓑햡썩촜펁턏했쿨', 'JOHAB'), 'JOHAB') = '벁먽듎핋쓑햡썩촜펁턏했쿨'
SELECT convert_to('꾻놪횜셺캸쏒뛡뮙삼改옋첮', 'JOHAB'), convert_from(convert_to('꾻놪횜셺캸쏒뛡뮙삼改옋첮', 'JOHAB'), 'JOHAB') = '꾻놪횜셺캸쏒뛡뮙삼改옋첮'
SELECT convert_from('\x788fbfef'::bytea, 'JOHAB')
SELECT convert_from('\x788fc8ea'::bytea, 'JOHAB')
SELECT convert_from('\xb44f84a1'::bytea, 'JOHAB')
SELECT convert_from('\xfa3d84a1'::bytea, 'JOHAB')
SELECT convert_from('\x84c180a15c783830'::bytea, 'JOHAB')
SELECT length('\x80a1'::bytea, 'JOHAB')
SELECT convert_from('\x84'::bytea, 'JOHAB')
SELECT convert_from('\x616285a100'::bytea, 'JOHAB')
SELECT convert_from('\x5c7838645c783230'::bytea, 'JOHAB')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'JOHAB')
SELECT length('\x8571'::bytea, 'JOHAB')
SELECT convert_to('aéb', 'JOHAB')
SELECT convert_to('😀', 'JOHAB')
SELECT convert_to('あ́', 'JOHAB')
# SHIFT_JIS_2004: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x618d7efa41f5c1e95bee4687bf8ce98386e3d9e1898458e7497a'::bytea, 'SHIFT_JIS_2004'), length('\x618d7efa41f5c1e95bee4687bf8ce98386e3d9e1898458e7497a'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\x6194a7fc5e9783f1c9f871eaf6fab6e5e391a2e8f19d6893f87a'::bytea, 'SHIFT_JIS_2004'), length('\x6194a7fc5e9783f1c9f871eaf6fab6e5e391a2e8f19d6893f87a'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\x6195d687eb8d74fb889373f6ef9c72f7479e74e2f38485904b7a'::bytea, 'SHIFT_JIS_2004'), length('\x6195d687eb8d74fb889373f6ef9c72f7479e74e2f38485904b7a'::bytea, 'SHIFT_JIS_2004')
SELECT convert_to('衢葙凧茛銈涔黌宣聳п鶖ɯ', 'SHIFT_JIS_2004'), convert_from(convert_to('衢葙凧茛銈涔黌宣聳п鶖ɯ', 'SHIFT_JIS_2004'), 'SHIFT_JIS_2004') = '衢葙凧茛銈涔黌宣聳п鶖ɯ'
SELECT convert_to('抔呰札競揩陦夛塀逆晢髮議', 'SHIFT_JIS_2004'), convert_from(convert_to('抔呰札競揩陦夛塀逆晢髮議', 'SHIFT_JIS_2004'), 'SHIFT_JIS_2004') = '抔呰札競揩陦夛塀逆晢髮議'
SELECT convert_to('䇦岊呉筈嚲再遄∬疼涊鞄探', 'SHIFT_JIS_2004'), convert_from(convert_to('䇦岊呉筈嚲再遄∬疼涊鞄探', 'SHIFT_JIS_2004'), 'SHIFT_JIS_2004') = '䇦岊呉筈嚲再遄∬疼涊鞄探'
SELECT convert_from('\x7882fb'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\x7882fa'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\xff7ba1'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\xffeea1'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\xa282fa5c783830'::bytea, 'SHIFT_JIS_2004')
SELECT length('\x82fa'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\xa3'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\x6162a400'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\x5c7838645c783230'::bytea, 'SHIFT_JIS_2004')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'SHIFT_JIS_2004')
SELECT length('\xa571'::bytea, 'SHIFT_JIS_2004')
SELECT convert_to('aéb', 'SHIFT_JIS_2004')
SELECT convert_to('😀', 'SHIFT_JIS_2004')
SELECT convert_to('あ́', 'SHIFT_JIS_2004')
# EUC_JIS_2004: characters out and back, what has no mapping, and what its verifier refuses.
SELECT convert_from('\x61aacda8a3ece9f9f2f1d7b7bff6e6dee68ffdaf8fa8d5d1e8d5cc7a'::bytea, 'EUC_JIS_2004'), length('\x61aacda8a3ece9f9f2f1d7b7bff6e6dee68ffdaf8fa8d5d1e8d5cc7a'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x61faf3cfe4aefeaedca7d4cde08fefc3afa58ff5dacacbacc98fa3a97a'::bytea, 'EUC_JIS_2004'), length('\x61faf3cfe4aefeaedca7d4cde08fefc3afa58ff5dacacbacc98fa3a97a'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x61a7c2d0e7accca2b0cdcdeed9c2a3c9c1f4eb8ff4a6cfe58ff6f87a'::bytea, 'EUC_JIS_2004'), length('\x61a7c2d0e7accca2b0cdcdeed9c2a3c9c1f4eb8ff4a6cfe58ff6f87a'::bytea, 'EUC_JIS_2004')
SELECT convert_to('娉牛酬俘皚蜷喆扼惱鱫嫐斤', 'EUC_JIS_2004'), convert_from(convert_to('娉牛酬俘皚蜷喆扼惱鱫嫐斤', 'EUC_JIS_2004'), 'EUC_JIS_2004') = '娉牛酬俘皚蜷喆扼惱鱫嫐斤'
SELECT convert_to('營磦晁鐻裃傾磨蕢蠕𤇆汶株', 'EUC_JIS_2004'), convert_from(convert_to('營磦晁鐻裃傾磨蕢蠕𤇆汶株', 'EUC_JIS_2004'), 'EUC_JIS_2004') = '營磦晁鐻裃傾磨蕢蠕𤇆汶株'
SELECT convert_to('㓁珺晝偈綠鮄継尹弋礜郤失', 'EUC_JIS_2004'), convert_from(convert_to('㓁珺晝偈綠鮄継尹弋礜郤失', 'EUC_JIS_2004'), 'EUC_JIS_2004') = '㓁珺晝偈綠鮄継尹弋礜郤失'
SELECT convert_from('\x788fbbf1'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x788fb5a7'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x8ef4a1a1'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x8ee9a1a1'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\xa1a2a4fc5c783830'::bytea, 'EUC_JIS_2004')
SELECT length('\xa4fc'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\xa1'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x6162a1a400'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x5c7838645c783230'::bytea, 'EUC_JIS_2004')
SELECT convert_from('\x5c7866665c7866665c7866665c786666'::bytea, 'EUC_JIS_2004')
SELECT length('\xa171'::bytea, 'EUC_JIS_2004')
SELECT convert_to('aéb', 'EUC_JIS_2004')
SELECT convert_to('😀', 'EUC_JIS_2004')
SELECT convert_to('あ́', 'EUC_JIS_2004')
# The pairs PostgreSQL converts between directly, by its own functions rather than through Unicode.
SELECT convert('\x8fb6c18fd6ac8faefe8ff1eea8a98fe4b58fcdfbf0aa8fa1cea6e7'::bytea, 'EUC_JP', 'SJIS')
SELECT convert('\x8fe2a88ff3cca6c4a5eb8fe0dc8fd3b5b9bf8fedc9f7d7a1dd'::bytea, 'EUC_JP', 'SJIS')
SELECT convert('\xdeb3b5a6d3a28fdff28fe4d6e8e3c9cb8ffbf78fc1c8a9a2'::bytea, 'EUC_JP', 'SJIS')
SELECT convert('\xa1a18ef8'::bytea, 'EUC_JP', 'SJIS')
SELECT convert('\xa1a18ef9'::bytea, 'EUC_JP', 'SJIS')
SELECT convert('\xa1a18eef'::bytea, 'EUC_JP', 'SJIS')
SELECT convert('\x91abf585f4fa93b685619fcd934ae5bb91bc85ab'::bytea, 'SJIS', 'EUC_JP')
SELECT convert('\x86519442e5b78c8d8c4693a8fc5be67cee9491e1'::bytea, 'SJIS', 'EUC_JP')
SELECT convert('\xf1b998cf955ef670e785ecc48cd187c58346e26b'::bytea, 'SJIS', 'EUC_JP')
SELECT convert('\xa1ffdf'::bytea, 'SJIS', 'EUC_JP')
SELECT convert('\xa18045'::bytea, 'SJIS', 'EUC_JP')
SELECT convert('\xa1fff1'::bytea, 'SJIS', 'EUC_JP')
SELECT convert('\xccd78ea1dba28ea1e7f68ea2dfbb8ea1a5a48ea2d8a1e0d1dafb8ea2d0d88ea1f8a5'::bytea, 'EUC_TW', 'BIG5')
SELECT convert('\xf0ac8ea2b9b58ea2e4ab8ea2e2b08ea2a3d68ea2d4d88ea2d8eccadee8df8ea2cfd0'::bytea, 'EUC_TW', 'BIG5')
SELECT convert('\x8ea2aad48ea2f2bc8ea1cdf08ea1a4da8ea2d2fc8ea2c3f48ea2d4b68ea2d0f8dae88ea1d0fb'::bytea, 'EUC_TW', 'BIG5')
SELECT convert('\xa1a18ea6b2f8'::bytea, 'EUC_TW', 'BIG5')
SELECT convert('\xa1a18eb0bace'::bytea, 'EUC_TW', 'BIG5')
SELECT convert('\xa1a18eafaef9'::bytea, 'EUC_TW', 'BIG5')
SELECT convert('\xec52c062bbdfd285b324e745b8cfce60f13fe769'::bytea, 'BIG5', 'EUC_TW')
SELECT convert('\xae75e358e3b9f4e7dfc0ec47bf1ad508e7c4d7ca'::bytea, 'BIG5', 'EUC_TW')
SELECT convert('\xe20cae2be4a1c9dea2bdf039d576b1c5cee0d049'::bytea, 'BIG5', 'EUC_TW')
SELECT convert('\xa1409920'::bytea, 'BIG5', 'EUC_TW')
SELECT convert('\xa140991d'::bytea, 'BIG5', 'EUC_TW')
SELECT convert('\xa14086a4'::bytea, 'BIG5', 'EUC_TW')
SELECT convert('\xfdcffde2aad7a8f6f8fea1ebd4eccfd38ff7a8f9e6'::bytea, 'EUC_JIS_2004', 'SHIFT_JIS_2004')
SELECT convert('\xe8c0fcedc0cec4c6ced3a8cec7c6f3fbdfe68ff1c3'::bytea, 'EUC_JIS_2004', 'SHIFT_JIS_2004')
SELECT convert('\xb0cb8ff6daefaed4c3a5e4b7d08ff9e4c9a6d0fab7c0'::bytea, 'EUC_JIS_2004', 'SHIFT_JIS_2004')
SELECT convert('\xa1a18fc7f3'::bytea, 'EUC_JIS_2004', 'SHIFT_JIS_2004')
SELECT convert('\xa1a18fe4f7'::bytea, 'EUC_JIS_2004', 'SHIFT_JIS_2004')
SELECT convert('\xa1a18fb1b8'::bytea, 'EUC_JIS_2004', 'SHIFT_JIS_2004')
SELECT convert('\x9d769b8b92d791ece2a98eaae78498da82e8f19c'::bytea, 'SHIFT_JIS_2004', 'EUC_JIS_2004')
SELECT convert('\x83c9f74294ee86b4f84f9781e0cd8999e68484e1'::bytea, 'SHIFT_JIS_2004', 'EUC_JIS_2004')
SELECT convert('\xf972efed9b77e7f890749846936e945e9fe1fbdc'::bytea, 'SHIFT_JIS_2004', 'EUC_JIS_2004')
SELECT convert('\xa1fe6a'::bytea, 'SHIFT_JIS_2004', 'EUC_JIS_2004')
SELECT convert('\xa1ff69'::bytea, 'SHIFT_JIS_2004', 'EUC_JIS_2004')
SELECT convert('\xa1fdb3'::bytea, 'SHIFT_JIS_2004', 'EUC_JIS_2004')
# What has nothing to convert, and the pairs with no conversion function.
SELECT convert('\xa4a2'::bytea, 'EUC_JP', 'EUC_JP'), convert('\xa4a2'::bytea, 'EUC_JP', 'SQL_ASCII'), convert(''::bytea, 'EUC_JP', 'LATIN1')
SELECT convert('\xa4'::bytea, 'EUC_JP', 'EUC_JP')
SELECT convert('\xa4'::bytea, 'EUC_JP', 'SQL_ASCII')
SELECT convert('\xe9'::bytea, 'SQL_ASCII', 'EUC_JP')
SELECT convert('\xa4a2'::bytea, 'SQL_ASCII', 'EUC_JP'), convert('\x82a0'::bytea, 'SQL_ASCII', 'SJIS'), convert('\x82a0'::bytea, 'SJIS', 'SQL_ASCII')
SELECT convert('\x8130'::bytea, 'SQL_ASCII', 'GB18030')
SELECT convert('\x00'::bytea, 'LATIN1', 'EUC_JP')
SELECT convert('a'::bytea, 'EUC_JP', 'LATIN1')
SELECT convert('a'::bytea, 'EUC_JP', 'EUC_KR')
SELECT convert('a'::bytea, 'GBK', 'GB18030')
SELECT convert('a'::bytea, 'LATIN2', 'SJIS')
SELECT convert('\xe38182'::bytea, 'UTF8', 'SJIS'), convert('\x82a0'::bytea, 'SJIS', 'UTF8'), convert('\xa4a2'::bytea, 'EUC_JP', 'utf-8')
SELECT convert('\xc3'::bytea, 'UTF8', 'SJIS')
SELECT convert_to('あ', 'Shift_JIS'), convert_to('あ', 'mskanji'), convert_to('あ', 'win932'), convert_to('中', 'windows936'), convert_to('한', 'win949'), convert_to('中', 'win950'), convert_to('あ', 'eucjp')
SELECT pg_typeof(convert_to('あ', 'SJIS')), pg_typeof(convert_from('\x82a0', 'SJIS')), pg_typeof(length('\x82a0'::bytea, 'SJIS'))
SELECT convert_from(NULL, 'SJIS'), convert_to(NULL, 'SJIS'), length(NULL::bytea, 'SJIS')
SELECT convert_to('か゚き゚', 'SHIFT_JIS_2004'), convert_to('か゚', 'EUC_JIS_2004'), convert_to('か゚', 'SJIS')
SELECT convert_to('𠀀㐀', 'GB18030'), convert_from('\x9030813082358f33', 'GB18030'), convert_to('ђ‏', 'GB18030')
SELECT convert_from('\xe3329a35', 'GB18030'), convert_from('\x84318730', 'GB18030')
SELECT convert_from('\xe3329a36', 'GB18030')
SELECT convert_to('가', 'JOHAB'), convert_from(convert_to('가', 'JOHAB'), 'JOHAB')
# client_encoding: a query's text and its results in the client's encoding.
SET client_encoding = 'SJIS'
SHOW client_encoding
SELECT 'あ表ｱ', length('あ表ｱ'), octet_length('あ表ｱ'), 'あ表ｱ' || 'x' AS "あ表ｱ"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('あ表ｱ', 'UTF8')
SET client_encoding = 'EUC_JP'
SHOW client_encoding
SELECT 'あ漢字', length('あ漢字'), octet_length('あ漢字'), 'あ漢字' || 'x' AS "あ漢字"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('あ漢字', 'UTF8')
SET client_encoding = 'EUC_CN'
SHOW client_encoding
SELECT '中文', length('中文'), octet_length('中文'), '中文' || 'x' AS "中文"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('中文', 'UTF8')
SET client_encoding = 'EUC_KR'
SHOW client_encoding
SELECT '한국어', length('한국어'), octet_length('한국어'), '한국어' || 'x' AS "한국어"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('한국어', 'UTF8')
SET client_encoding = 'GBK'
SHOW client_encoding
SELECT '中文龘', length('中文龘'), octet_length('中文龘'), '中文龘' || 'x' AS "中文龘"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('中文龘', 'UTF8')
SET client_encoding = 'UHC'
SHOW client_encoding
SELECT '한국어갂', length('한국어갂'), octet_length('한국어갂'), '한국어갂' || 'x' AS "한국어갂"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('한국어갂', 'UTF8')
SET client_encoding = 'BIG5'
SHOW client_encoding
SELECT '中文繁', length('中文繁'), octet_length('中文繁'), '中文繁' || 'x' AS "中文繁"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('中文繁', 'UTF8')
SET client_encoding = 'GB18030'
SHOW client_encoding
SELECT '中𠀀€', length('中𠀀€'), octet_length('中𠀀€'), '中𠀀€' || 'x' AS "中𠀀€"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('中𠀀€', 'UTF8')
SET client_encoding = 'JOHAB'
SHOW client_encoding
SELECT '中', length('中'), octet_length('中'), '中' || 'x' AS "中"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('中', 'UTF8')
SET client_encoding = 'SHIFT_JIS_2004'
SHOW client_encoding
SELECT 'か゚あ', length('か゚あ'), octet_length('か゚あ'), 'か゚あ' || 'x' AS "か゚あ"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('か゚あ', 'UTF8')
SET client_encoding = 'EUC_JIS_2004'
SHOW client_encoding
SELECT 'か゚表', length('か゚表'), octet_length('か゚表'), 'か゚表' || 'x' AS "か゚表"
SELECT chr(233)
SELECT chr(128512)
SELECT convert_to('か゚表', 'UTF8')
RESET client_encoding
SHOW client_encoding
